use std::sync::Arc;

use anyhow::{Result, anyhow};
use gpui::{App, SharedString, Task, WeakEntity};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::thread::{Message, Thread};
use crate::{AgentTool, ToolCallEventStream};
use language_model::Role;

/// Input for the token usage tool.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TokenUsageToolInput {
    /// When true, include per-message token counts (precise if available, otherwise heuristic).
    #[serde(default)]
    pub include_per_message: bool,
    /// When true, include archived memory segment breakdown and aggregate savings.
    #[serde(default)]
    pub include_memory_breakdown: bool,
}

/// Tool output is markdown text.
type TokenUsageToolOutput = String;

/// Tool that reports current thread token usage metrics (active vs full, memory savings,
/// and optional per-message / memory segment breakdowns).
pub struct TokenUsageTool {
    thread: WeakEntity<Thread>,
}

impl TokenUsageTool {
    pub fn new(thread: WeakEntity<Thread>) -> Self {
        Self { thread }
    }

    fn heuristic_per_message_tokens(messages: &[Message]) -> Vec<usize> {
        use crate::token_usage::heuristic_token_count;
        use language_model::LanguageModelRequestMessage;

        messages
            .iter()
            .map(|m| {
                let req: Vec<LanguageModelRequestMessage> = m.to_request();
                heuristic_token_count(&req)
            })
            .collect()
    }
}

impl AgentTool for TokenUsageTool {
    type Input = TokenUsageToolInput;
    type Output = TokenUsageToolOutput;

    fn name() -> &'static str {
        "token_usage"
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
            Ok(cfg) if cfg.include_per_message => "Token usage (detailed)".into(),
            _ => "Token usage".into(),
        }
    }

    fn run(
        self: Arc<Self>,
        input: Self::Input,
        _event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output>> {
        let Some(thread) = self.thread.upgrade() else {
            return Task::ready(Ok("# Token Usage\n\nThread no longer exists.\n".into()));
        };

        // Gather core metrics in a single read.
        let (
            active_usage_opt,
            full_usage_opt,
            precise_opt,
            precise_max_opt,
            precise_per_opt,
            metas,
        ) = thread.read_with(cx, |t, _| {
            (
                t.active_and_full_token_usage()
                    .map(|(active, _full)| active),
                t.active_and_full_token_usage().map(|(_active, full)| full),
                t.precise_active_tokens,
                t.precise_max_tokens,
                t.precise_per_message_tokens.clone(),
                t.memory_segment_metas(),
            )
        });

        // Compute memory savings aggregate.
        let memory_segments_count = metas.len();
        let memory_saved_tokens: usize = metas.iter().map(|m| m.6).sum();

        // Active vs full tokens (prefer precise for active if available).
        let (active_used, active_max, active_precise) =
            match (active_usage_opt.as_ref(), precise_opt, precise_max_opt) {
                (Some(active_usage), Some(precise_used), Some(precise_max)) => {
                    (precise_used, precise_max, true)
                }
                (Some(active_usage), _, _) => {
                    (active_usage.used_tokens, active_usage.max_tokens, false)
                }
                _ => (0, 0, false),
            };

        let (full_used, full_max) = match full_usage_opt {
            Some(full_usage) => (full_usage.used_tokens, full_usage.max_tokens),
            None => (0, active_max),
        };

        let active_pct = if active_max > 0 {
            (active_used as f64 / active_max as f64 * 100.0)
        } else {
            0.0
        };
        let full_pct = if full_max > 0 {
            (full_used as f64 / full_max as f64 * 100.0)
        } else {
            0.0
        };

        // Per-message token counts (precise or heuristic).
        let per_message_tokens = if input.include_per_message {
            match precise_per_opt {
                Some(ref v) if !v.is_empty() => Some(v.clone()),
                _ => {
                    // Fallback heuristic
                    let heuristics = thread
                        .read_with(cx, |t, _| Self::heuristic_per_message_tokens(t.messages()));
                    Some(heuristics)
                }
            }
        } else {
            None
        };

        // Construct JSON payload.
        let core_json = serde_json::json!({
            "active_tokens_used": active_used,
            "active_tokens_max": active_max,
            "active_usage_pct": (active_pct * 100.0).round() / 100.0,
            "active_is_precise": active_precise,
            "full_tokens_used": full_used,
            "full_tokens_max": full_max,
            "full_usage_pct": (full_pct * 100.0).round() / 100.0,
            "memory_segment_count": memory_segments_count,
            "memory_saved_tokens_estimate": memory_saved_tokens,
        });

        // Build markdown output.
        let mut md = String::new();
        md.push_str("# Token Usage\n\n```json\n");
        match serde_json::to_string_pretty(&core_json) {
            Ok(pretty) => {
                md.push_str(&pretty);
            }
            Err(e) => {
                return Task::ready(Err(anyhow!("failed to serialize core usage: {e}")));
            }
        }
        md.push_str("\n```\n");

        if input.include_per_message {
            if let Some(tokens) = per_message_tokens {
                let messages_data: Vec<serde_json::Value> = thread.read_with(cx, |t, _| {
                    t.messages()
                        .iter()
                        .zip(tokens.iter())
                        .enumerate()
                        .map(|(idx, (msg, tok))| {
                            let role = match msg.role() {
                                Role::User => "user",
                                Role::Assistant => "assistant",
                                Role::System => "system",
                            };
                            // Short preview (avoid dumping full content; memory segments may aggregate large content)
                            let preview = match msg {
                                Message::User(u) => u
                                    .content
                                    .iter()
                                    .filter_map(|c| match c {
                                        crate::thread::UserMessageContent::Text(t) => {
                                            Some(t.as_str())
                                        }
                                        _ => None,
                                    })
                                    .take(1)
                                    .collect::<Vec<_>>()
                                    .join(" "),
                                Message::Agent(a) => a
                                    .content
                                    .iter()
                                    .filter_map(|c| match c {
                                        crate::thread::AgentMessageContent::Text(t) => {
                                            Some(t.as_str())
                                        }
                                        _ => None,
                                    })
                                    .take(1)
                                    .collect::<Vec<_>>()
                                    .join(" "),
                                Message::Resume => "Resume".into(),
                            };
                            let trimmed = if preview.len() > 96 {
                                format!("{}…", &preview[..96])
                            } else {
                                preview
                            };
                            serde_json::json!({
                                "index": idx,
                                "role": role,
                                "tokens": tok,
                                "preview": trimmed
                            })
                        })
                        .collect()
                });

                md.push_str("\n## Per-Message Tokens\n\n```json\n");
                match serde_json::to_string_pretty(&messages_data) {
                    Ok(pretty) => md.push_str(&pretty),
                    Err(e) => {
                        return Task::ready(Err(anyhow!(
                            "failed to serialize per-message tokens: {e}"
                        )));
                    }
                }
                md.push_str("\n```\n");
            }
        }

        if input.include_memory_breakdown && !metas.is_empty() {
            let memory_json: Vec<serde_json::Value> = metas
                .iter()
                .map(|m| {
                    serde_json::json!({
                        "id": m.0,
                        "start": m.1,
                        "end": m.2,
                        "message_count": m.3,
                        "message_chars": m.4,
                        "placeholder_chars": m.5,
                        "token_savings_estimate": m.6,
                        "summary": m.7,
                        "stored_epoch_ms": m.8
                    })
                })
                .collect();
            md.push_str("\n## Memory Segments\n\n```json\n");
            match serde_json::to_string_pretty(&memory_json) {
                Ok(pretty) => md.push_str(&pretty),
                Err(e) => {
                    return Task::ready(Err(anyhow!(
                        "failed to serialize memory segment breakdown: {e}"
                    )));
                }
            }
            md.push_str("\n```\n");
        }

        Task::ready(Ok(md))
    }
}
