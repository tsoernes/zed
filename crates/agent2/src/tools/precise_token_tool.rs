use std::sync::Arc;

use anyhow::{anyhow, Result};
use gpui::{App, SharedString, Task, WeakEntity};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::thread::Thread;
use crate::{AgentTool, ToolCallEventStream};

use language_model::{LanguageModelRequest, LanguageModelRequestMessage};

/// Input for the precise token counting tool.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PreciseTokenToolInput {
    /// When true, return per-message precise token counts in addition to the total.
    #[serde(default)]
    pub include_per_message: bool,

    /// Optional start index (inclusive) for the message slice to measure.
    /// If omitted, measurement uses the entire active message sequence.
    #[serde(default)]
    pub start: Option<usize>,

    /// Optional end index (exclusive) for the message slice to measure.
    /// If omitted, measurement uses the entire active message sequence.
    #[serde(default)]
    pub end: Option<usize>,
}

/// Tool output is markdown describing precise token counts.
type PreciseTokenToolOutput = String;

/// Tool that computes precise token counts for a thread's message history (total
/// and optional per-message breakdown). This tries to use the model-backed
/// precise counting facility and falls back to heuristics where necessary.
pub struct PreciseTokenTool {
    thread: WeakEntity<Thread>,
}

impl PreciseTokenTool {
    pub fn new(thread: WeakEntity<Thread>) -> Self {
        Self { thread }
    }
}

impl AgentTool for PreciseTokenTool {
    type Input = PreciseTokenToolInput;
    type Output = PreciseTokenToolOutput;

    fn name() -> &'static str {
        "precise_tokens"
    }

    fn kind() -> agent_client_protocol::ToolKind {
        agent_client_protocol::ToolKind::Read
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        match input {
            Ok(cfg) if cfg.include_per_message => "Precise token usage (detailed)".into(),
            _ => "Precise token usage".into(),
        }
    }

    fn run(
        self: Arc<Self>,
        input: Self::Input,
        _event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output>> {
        // Upgrade thread handle
        let Some(thread) = self.thread.upgrade() else {
            return Task::ready(Ok("# Precise Tokens\n\nThread no longer exists.\n".into()));
        };

        // Snapshot required thread state in a single read to avoid races.
        let (model_opt, prompt_id_opt, completion_mode_opt, msgs_snapshot): (
            Option<Arc<dyn language_model::LanguageModel>>,
            Option<String>,
            Option<agent_settings::CompletionMode>,
            Vec<LanguageModelRequestMessage>,
        ) = thread.read_with(cx, |t, _| {
            (
                t.model.clone(),
                Some(t.prompt_id.to_string()),
                Some(t.completion_mode),
                t.messages().iter().flat_map(|m| m.to_request()).collect::<Vec<_>>(),
            )
        });

        // If there's no configured model, precise counting isn't available.
        let model = match model_opt {
            Some(m) => m,
            None => {
                return Task::ready(Ok(
                    "# Precise Tokens\n\nNo language model configured for this thread; precise counting unavailable.\n".into(),
                ))
            }
        };

        // Determine the message slice to measure.
        let total_msgs = msgs_snapshot.len();
        let start = input.start.unwrap_or(0).min(total_msgs);
        let end = input.end.unwrap_or(total_msgs).min(total_msgs);

        if start >= end {
            return Task::ready(Ok(format!(
                "# Precise Tokens\n\nInvalid or empty slice requested: start={} end={} (total messages: {})\n",
                start, end, total_msgs
            )));
        }

        // Use the requested slice for measurement.
        let slice: Vec<LanguageModelRequestMessage> = msgs_snapshot[start..end].to_vec();

        // Build a LanguageModelRequest base similar to how the thread does it for counting.
        let base_request = LanguageModelRequest {
            thread_id: prompt_id_opt.as_ref().map(|s| s.clone()), // optional, not required
            prompt_id: prompt_id_opt.clone(),
            intent: None,
            mode: completion_mode_opt.map(|m| m.into()),
            messages: slice.clone(),
            tools: Vec::new(),
            tool_choice: None,
            stop: Vec::new(),
            temperature: Some(0.0),
            thinking_allowed: true,
        };

        // Spawn an async foreground task that has access to an async App context.
        // This mirrors how other precise counting flows run inside the app's async runtime.
        cx.spawn(async move |_, cx| {
            // Try the precise per-message computation first (may be expensive).
            // The precise helpers will fall back to heuristics if the provider doesn't support counting.
            match crate::token_usage::precise_per_message_tokens(&model, &base_request, &slice, cx).await {
                Ok((per_message, total_precise)) => {
                    // Build JSON payload
                    let core = serde_json::json!({
                        "total_precise_tokens": total_precise,
                        "message_count": slice.len(),
                        "measured_range": format!("{}..{}", start, end),
                    });

                    let mut md = String::new();
                    md.push_str("# Precise Token Counts\n\n```json\n");
                    md.push_str(&serde_json::to_string_pretty(&core).unwrap_or_else(|_| "{}".into()));
                    md.push_str("\n```\n");

                    if input.include_per_message {
                        // Prepare per-message JSON with indices relative to the slice start.
                        let per_json: Vec<serde_json::Value> = per_message
                            .into_iter()
                            .enumerate()
                            .map(|(i, tok)| {
                                serde_json::json!({
                                    "index": start + i,
                                    "tokens": tok
                                })
                            })
                            .collect();

                        md.push_str("\n## Per-Message Tokens\n\n```json\n");
                        md.push_str(&serde_json::to_string_pretty(&per_json).unwrap_or_else(|_| "[]".into()));
                        md.push_str("\n```\n");
                    }

                    Ok(md)
                }
                Err(e) => {
                    // If precise per-message failed, attempt a precise total for the slice.
                    let total_precise = crate::token_usage::precise_tokens_for_slice(&model, &base_request, &slice, cx).await;
                    // Fallback to heuristic if precise total also failed or returned 0.
                    let final_total = if total_precise == 0 {
                        crate::token_usage::heuristic_token_count(&slice)
                    } else {
                        total_precise
                    };

                    let core = serde_json::json!({
                        "total_precise_tokens (best_effort)": final_total,
                        "message_count": slice.len(),
                        "measured_range": format!("{}..{}", start, end),
                        "note": format!("precise_per_message_tokens failed: {}", e),
                    });

                    let mut md = String::new();
                    md.push_str("# Precise Token Counts (best-effort)\n\n```json\n");
                    md.push_str(&serde_json::to_string_pretty(&core).unwrap_or_else(|_| "{}".into()));
                    md.push_str("\n```\n");

                    if input.include_per_message {
                        md.push_str("\nNote: per-message precise counts unavailable; returning heuristic per-message estimate.\n\n");
                        let heur = crate::token_usage::heuristic_per_message(&slice);
                        let per_json: Vec<serde_json::Value> = heur
                            .into_iter()
                            .enumerate()
                            .map(|(i, tok)| {
                                serde_json::json!({
                                    "index": start + i,
                                    "tokens_estimate": tok
                                })
                            })
                            .collect();
                        md.push_str("\n```json\n");
                        md.push_str(&serde_json::to_string_pretty(&per_json).unwrap_or_else(|_| "[]".into()));
                        md.push_str("\n```\n");
                    }

                    Ok(md)
                }
            }
        })
    }
}
