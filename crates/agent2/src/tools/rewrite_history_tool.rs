use crate::{AgentTool, Thread, ToolCallEventStream, thread::Message};
use acp_thread::UserMessageId;
use agent_client_protocol as acp;
use anyhow::{Result, anyhow};
use gpui::{App, Entity, SharedString, Task, WeakEntity};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use crate::thread::{UserMessage, UserMessageContent};

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
        } = input;

        if start_index > end_index {
            return Err(anyhow!(
                "start_index ({start_index}) was greater than end_index ({end_index})"
            ));
        }
        if end_index >= thread.messages.len() {
            return Err(anyhow!(
                "end_index ({end_index}) out of bounds (messages len = {})",
                thread.messages.len()
            ));
        }

        let range_len = end_index - start_index + 1;

        // Collect original markdown
        let mut original = String::new();
        for (ix, message) in thread
            .messages
            .iter()
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

        thread
            .messages
            .splice(start_index..=end_index, std::iter::once(replacement));

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

        cx.update(|cx| {
            thread.update(cx, |thread, _cx| {
                let (result_msg, placeholder) = self.rewrite(thread, input)?;
                event_stream.update_fields(acp::ToolCallUpdateFields {
                    content: Some(vec![result_msg.clone().into(), placeholder.into()]),
                    ..Default::default()
                });
                Ok(result_msg)
            })
        })
    }
}
