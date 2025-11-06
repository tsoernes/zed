use std::sync::Arc;

use action_log::ActionLog;
use anyhow::Result;
use assistant_tool::{Tool, ToolResult};
use gpui::{AnyWindowHandle, App, Entity, Task};
use language_model::{
    LanguageModel, LanguageModelRequest, LanguageModelRequestMessage,
    LanguageModelToolSchemaFormat, MessageContent,
};
use project::Project;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use ui::IconName;

/// Input for the token usage adapter tool.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TokenUsageAdapterInput {
    /// Include per-message token counts (precise when available, else heuristic fallback).
    #[serde(default)]
    pub include_per_message: bool,
    /// Include memory segment breakdown (not yet supported; returns placeholders if true).
    #[serde(default)]
    pub include_memory_breakdown: bool,
}

/// Adapter tool that reports token usage for the current request context.
/// This wraps the existing LanguageModelRequest passed to `run` and uses the model's `count_tokens`
/// method for precise counting. If precise counting fails, it falls back to a heuristic (char/4).
pub struct TokenUsageAdapterTool;

impl TokenUsageAdapterTool {
    pub const NAME: &str = "token_usage";

    fn heuristic_token_count(messages: &[LanguageModelRequestMessage]) -> usize {
        messages
            .iter()
            .map(|m| {
                m.content
                    .iter()
                    .map(|c| match c {
                        MessageContent::Text(t) => t.len(),
                        MessageContent::Thinking { text, .. } => text.len(),
                        MessageContent::RedactedThinking(t) => t.len(),
                        // Rough placeholders for non-text content.
                        MessageContent::Image(_) => 7,
                        MessageContent::ToolUse(_) => 11,
                        MessageContent::ToolResult(_) => 14,
                    })
                    .sum::<usize>()
            })
            .sum::<usize>()
            .saturating_div(4)
            .max(1)
    }

    fn build_request(
        base: &LanguageModelRequest,
        slice: &[LanguageModelRequestMessage],
    ) -> LanguageModelRequest {
        LanguageModelRequest {
            thread_id: base.thread_id.clone(),
            prompt_id: base.prompt_id.clone(),
            intent: base.intent.clone(),
            mode: base.mode.clone(),
            messages: slice.to_vec(),
            tools: base.tools.clone(),
            tool_choice: base.tool_choice.clone(),
            stop: base.stop.clone(),
            temperature: base.temperature,
            thinking_allowed: base.thinking_allowed,
        }
    }

    fn precise_tokens_for_slice(
        model: &Arc<dyn LanguageModel>,
        base: &LanguageModelRequest,
        slice: &[LanguageModelRequestMessage],
        cx: &App,
    ) -> Option<u64> {
        if slice.is_empty() {
            return Some(0);
        }
        let req = Self::build_request(base, slice);
        // Do not block if provider refuses counting; return None to fallback.
        match futures::executor::block_on(model.count_tokens(req, cx)) {
            Ok(v) if v > 0 => Some(v),
            _ => None,
        }
    }

    fn compute_precise_per_message(
        model: &Arc<dyn LanguageModel>,
        base: &LanguageModelRequest,
        all: &[LanguageModelRequestMessage],
        cx: &App,
    ) -> Option<(Vec<u64>, u64)> {
        if all.is_empty() {
            return Some((Vec::new(), 0));
        }
        let mut prefix_totals: Vec<u64> = Vec::with_capacity(all.len() + 1);
        prefix_totals.push(0);
        for i in 0..all.len() {
            let slice = &all[..=i];
            let total = Self::precise_tokens_for_slice(model, base, slice, cx)?; // Abort on first failure.
            prefix_totals.push(total);
        }
        let mut per: Vec<u64> = Vec::with_capacity(all.len());
        for i in 0..all.len() {
            per.push(prefix_totals[i + 1].saturating_sub(prefix_totals[i]));
        }
        Some((per, *prefix_totals.last().unwrap_or(&0)))
    }
}

impl Tool for TokenUsageAdapterTool {
    fn name(&self) -> String {
        Self::NAME.to_string()
    }

    fn needs_confirmation(
        &self,
        _input: &serde_json::Value,
        _project: &Entity<Project>,
        _cx: &App,
    ) -> bool {
        false
    }

    fn may_perform_edits(&self) -> bool {
        false
    }

    fn description(&self) -> String {
        "Report current context token usage (active vs full projection) with precise counting when supported.".into()
    }

    fn icon(&self) -> IconName {
        IconName::Info
    }

    fn input_schema(&self, format: LanguageModelToolSchemaFormat) -> Result<serde_json::Value> {
        crate::schema::json_schema_for::<TokenUsageAdapterInput>(format)
    }

    fn ui_text(&self, _input: &serde_json::Value) -> String {
        "Token usage summary".into()
    }

    fn run(
        self: Arc<Self>,
        input: serde_json::Value,
        request: Arc<LanguageModelRequest>,
        _project: Entity<Project>,
        _action_log: Entity<ActionLog>,
        model: Arc<dyn LanguageModel>,
        _window: Option<AnyWindowHandle>,
        cx: &mut App,
    ) -> ToolResult {
        let parsed: TokenUsageAdapterInput = match serde_json::from_value(input) {
            Ok(v) => v,
            Err(e) => {
                return ToolResult {
                    output: Task::ready(Ok(json!({ "error": format!("invalid input: {e}") })
                        .to_string()
                        .into())),
                    card: None,
                };
            }
        };

        // Active context = messages currently in request.
        let messages = &request.messages;

        // Precise active token count.
        let precise_active_opt = Self::precise_tokens_for_slice(&model, &request, messages, cx);
        let active_used = precise_active_opt
            .map(|v| v as u64)
            .unwrap_or_else(|| Self::heuristic_token_count(messages) as u64);

        // We treat full context as active + hypothetical expansion of archived data.
        // Since assistant_tools does not track archived segments, we report full == active.
        let full_used = active_used;

        // Max tokens (best effort). If unsupported, leave None.
        let max_tokens = model.max_token_count();

        // Per-message precise counts if requested.
        let per_message_tokens: Option<Vec<u64>> = if parsed.include_per_message {
            match Self::compute_precise_per_message(&model, &request, messages, cx) {
                Some((per, _total)) if !per.is_empty() => Some(per),
                _ => {
                    // Heuristic fallback (char/4 per message).
                    let heuristics = messages
                        .iter()
                        .map(|m| Self::heuristic_token_count(std::slice::from_ref(m)) as u64)
                        .collect::<Vec<_>>();
                    Some(heuristics)
                }
            }
        } else {
            None
        };

        // Memory breakdown placeholder.
        let (memory_segment_count, memory_saved_tokens) = if parsed.include_memory_breakdown {
            // Not implemented here; agent2 internal memory segments unavailable.
            (0, 0_u64)
        } else {
            (0, 0_u64)
        };

        // Per-message JSON previews.
        let per_message_json = per_message_tokens.as_ref().map(|counts| {
            messages
                .iter()
                .zip(counts.iter())
                .enumerate()
                .map(|(idx, (msg, tok))| {
                    let role = match msg.role {
                        language_model::Role::User => "user",
                        language_model::Role::Assistant => "assistant",
                        language_model::Role::System => "system",
                    };
                    let preview = msg
                        .content
                        .iter()
                        .filter_map(|c| match c {
                            MessageContent::Text(t) => Some(t.as_ref()),
                            MessageContent::Thinking { text, .. } => Some(text.as_ref()),
                            MessageContent::RedactedThinking(t) => Some(t.as_ref()),
                            _ => None,
                        })
                        .take(1)
                        .collect::<Vec<_>>()
                        .join(" ");
                    let trimmed = if preview.len() > 96 {
                        format!("{}…", &preview[..96])
                    } else {
                        preview
                    };
                    json!({
                        "index": idx,
                        "role": role,
                        "tokens": tok,
                        "preview": trimmed
                    })
                })
                .collect::<Vec<_>>()
        });

        let active_pct = if max_tokens > 0 {
            (active_used as f64 / max_tokens as f64 * 10000.0).round() / 100.0
        } else {
            0.0
        };

        let core = json!({
            "active_tokens_used": active_used,
            "active_tokens_max": max_tokens,
            "active_usage_pct": active_pct,
            "active_is_precise": precise_active_opt.is_some(),
            "full_tokens_used": full_used,
            "full_tokens_max": max_tokens,
            "full_usage_pct": active_pct, // identical since full == active here
            "memory_segment_count": memory_segment_count,
            "memory_saved_tokens": memory_saved_tokens,
        });

        let mut output_obj = json!({ "core": core });
        if let Some(per) = per_message_json {
            output_obj
                .as_object_mut()
                .unwrap()
                .insert("per_message".into(), json!(per));
        }
        if parsed.include_memory_breakdown {
            output_obj.as_object_mut().unwrap().insert(
                "memory_segments".into(),
                json!({ "status": "not_available_in_adapter" }),
            );
        }

        ToolResult {
            output: Task::ready(Ok(output_obj.to_string().into())),
            card: None,
        }
    }
}
