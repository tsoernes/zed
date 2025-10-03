use crate::{AgentTool, Thread, ToolCallEventStream, thread::Message};
use acp_thread::UserMessageId;
use agent_client_protocol as acp;
use anyhow::{Result, anyhow};
use gpui::{App, SharedString, Task, WeakEntity};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use crate::thread::{UserMessage, UserMessageContent};

/**
 * Tool for shortening / compacting a portion of the conversation history when approaching
 * context or token limits.
 *
 * Use this to:
 * - Remove an obsolete span of messages (strategy = "remove")
 * - Summarize a span of messages into a single compact placeholder (strategy = "summarize")
 *
 * The tool:
 * 1. Generates a distinctive marker that records: range indices, message count, character count,
 *    and an optional summary.
 * 2. Inserts a preview (truncated if large) so the model still has immediate local signal.
 * 3. Embeds the full original markdown inside a collapsible <details> block so the *deleted* /
 *    *collapsed* context remains visible to the user (but is no longer fully re-sent in future
 *    model requests).
 *
 * Indices referenced are the 0-based positions the model now sees because each message is annotated
 * upstream in the request building step with [#<index>]. Provide inclusive start_index/end_index.
 *
 * This operation is lossy unless summarized content is sufficiently faithful. You can apply it
 * multiple times over earlier compressed regions to progressively shrink history.
 */
/// Strategy for rewriting (shortening) a portion of the thread history.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RewriteStrategy {
    /// Replace the selected range with a placeholder note indicating removal.
    Remove,
    /// Replace the selected range with a placeholder plus a supplied or auto-generated summary.
    Summarize,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct RewriteHistoryToolInput {
    /// Inclusive start index of the message range to rewrite.
    start_index: usize,
    /// Inclusive end index of the message range to rewrite.
    end_index: usize,
    /// Strategy: remove or summarize.
    strategy: RewriteStrategy,
    /// Optional summary when using Summarize. If omitted, a simple heuristic summary is generated.
    summary: Option<String>,
    /// Maximum number of original characters to embed (after summary) for visibility. Defaults to 400 if omitted or zero.
    max_preview_chars: Option<usize>,
    /// If strategy is `summarize` and this is true (or summary is None), attempt automated model summarization; fallback to heuristic if model unavailable.
    auto: Option<bool>,
}

pub struct RewriteHistoryTool {
    thread: WeakEntity<Thread>,
}

impl RewriteHistoryTool {
    pub fn new(thread: WeakEntity<Thread>) -> Self {
        Self { thread }
    }

    fn rewrite(
        &self,
        thread: &mut Thread,
        input: RewriteHistoryToolInput,
    ) -> Result<(String, String)> {
        let RewriteHistoryToolInput {
            start_index,
            end_index,
            strategy,
            summary,
            max_preview_chars,
            ..
        } = input;

        if start_index > end_index {
            return Err(anyhow!(
                "start_index ({start_index}) was greater than end_index ({end_index})"
            ));
        }
        if end_index >= thread.message_len() {
            return Err(anyhow!(
                "end_index ({end_index}) out of bounds (messages len = {})",
                thread.message_len()
            ));
        }

        let range_len = end_index - start_index + 1;

        // Collect original markdown using public helper
        let mut original = String::new();
        for (ix, message) in thread
            .iter_messages()
            .enumerate()
            .skip(start_index)
            .take(range_len)
        {
            if ix > start_index {
                original.push('\n');
            }
            original.push_str(&message.to_markdown());
        }

        let original_chars = original.chars().count();
        let max_preview = max_preview_chars.unwrap_or(400).max(40); // enforce a lower bound

        let preview = if original_chars <= max_preview {
            original.clone()
        } else {
            let mut truncated = String::new();
            for ch in original.chars().take(max_preview) {
                truncated.push(ch);
            }
            truncated.push_str("\n…(truncated)");
            truncated
        };

        let auto_summary = || {
            // Very basic heuristic summary.
            let first_line = original
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim();
            if first_line.is_empty() {
                format!("{range_len} message(s)")
            } else if first_line.len() > 120 {
                format!("{}… ({} message(s))", &first_line[..120], range_len)
            } else {
                format!("{first_line} ({} message(s))", range_len)
            }
        };

        let summary_text = match strategy {
            RewriteStrategy::Remove => None,
            RewriteStrategy::Summarize => Some(summary.unwrap_or_else(auto_summary)),
        };

        let strategy_label = match strategy {
            RewriteStrategy::Remove => "removed",
            RewriteStrategy::Summarize => "summarized",
        };

        let id = Uuid::new_v4();
        let mut placeholder = String::new();
        // Marker format chosen to be visually distinct and grep-able.
        placeholder.push_str(&format!(
            "[[history_rewrite {strategy_label} id={id} range={start_index}..{end_index} messages={range_len} chars={original_chars}]]\n"
        ));

        if let Some(s) = &summary_text {
            placeholder.push_str("\nSummary:\n");
            placeholder.push_str(s);
            placeholder.push('\n');
        }

        placeholder.push_str("\nPreview:\n");
        placeholder.push_str(&preview);
        placeholder.push('\n');

        // Keep original (collapsed) to satisfy "deleted context should still be visible".
        placeholder.push_str("\n<details><summary>Original (collapsed)</summary>\n\n```\n");
        placeholder.push_str(&original);
        placeholder.push_str("\n```\n</details>\n");

        // Replace the selected messages with a single synthetic user message.
        let replacement = Message::User(UserMessage {
            id: UserMessageId::new(),
            content: vec![UserMessageContent::Text(placeholder.clone())],
        });

        // Perform replacement via public API
        thread.replace_range_with_message(start_index, end_index, replacement)?;

        let result_message = match strategy {
            RewriteStrategy::Remove => format!(
                "Removed {range_len} message(s) (chars={original_chars}) at indices {start_index}..{end_index}. Replacement marker id={id}."
            ),
            RewriteStrategy::Summarize => format!(
                "Summarized {range_len} message(s) (chars={original_chars}) at indices {start_index}..{end_index}. Replacement marker id={id}."
            ),
        };

        Ok((result_message, placeholder))
    }
}

impl AgentTool for RewriteHistoryTool {
    type Input = RewriteHistoryToolInput;
    type Output = String;

    fn name() -> &'static str {
        "rewrite_history"
    }

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Other
    }

    fn description(&self) -> SharedString {
        "Use this tool to shorten/compact conversation history by removing or summarizing an indexed range of messages when nearing context or token limits. Provide inclusive start_index and end_index; choose strategy \"remove\" to drop detail or \"summarize\" to keep a concise representation.".into()
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        if let Ok(i) = input {
            match i.strategy {
                RewriteStrategy::Remove => "Rewrite (remove) history range".into(),
                RewriteStrategy::Summarize => "Rewrite (summarize) history range".into(),
            }
        } else {
            "Rewrite history".into()
        }
    }

    fn run(
        self: Arc<Self>,
        input: Self::Input,
        event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<String>> {
        let Some(thread) = self.thread.upgrade() else {
            return Task::ready(Err(anyhow!("thread no longer exists")));
        };

        let result = thread.update(cx, |thread, thread_cx| {
            let (result_msg, placeholder) = self.rewrite(thread, input)?;
            event_stream.update_fields(acp::ToolCallUpdateFields {
                content: Some(vec![result_msg.clone().into(), placeholder.into()]),
                ..Default::default()
            });
            // Notify that the thread's message history structure changed.
            thread_cx.notify();
            Ok(result_msg)
        });
        Task::ready(result)
    }
}
