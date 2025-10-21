use crate::{AgentTool, ToolCallEventStream};
use agent_client_protocol::ToolKind;
use anyhow::Result;
use gpui::{App, SharedString, Task};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Lists conversation messages with stable indices and previews for context management.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ListHistoryToolInput {
    /// Inclusive starting message index (default 0)
    #[serde(default)]
    start: usize,

    /// Number of messages to enumerate (default 40, clamped 1..500)
    #[serde(default = "default_limit")]
    limit: usize,

    /// Preview character cap per message (default 160, clamped 16..4096)
    #[serde(default = "default_max_chars")]
    max_chars_per_message: usize,

    /// If true, appends full text of each listed message after the table
    #[serde(default)]
    include_full_markdown: bool,
}

fn default_limit() -> usize {
    40
}

fn default_max_chars() -> usize {
    160
}

use crate::thread::Thread;
use gpui::WeakEntity;

pub struct ListHistoryTool {
    thread: WeakEntity<Thread>,
}

impl ListHistoryTool {
    pub fn new(thread: WeakEntity<Thread>) -> Self {
        Self { thread }
    }
}

impl AgentTool for ListHistoryTool {
    type Input = ListHistoryToolInput;
    type Output = String;

    fn name() -> &'static str {
        "list_history"
    }

    fn kind() -> ToolKind {
        ToolKind::Read
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        if let Ok(input) = input {
            format!(
                "List history (start: {}, limit: {})",
                input.start, input.limit
            )
            .into()
        } else {
            "List conversation history".into()
        }
    }

    fn run(
        self: Arc<Self>,
        mut input: Self::Input,
        _event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output>> {
        input.limit = input.limit.clamp(1, 500);
        input.max_chars_per_message = input.max_chars_per_message.clamp(16, 4096);

        let Some(thread) = self.thread.upgrade() else {
            return Task::ready(Ok(String::from(
                "# Conversation History\n\nThread no longer exists.\n",
            )));
        };

        // Read thread state
        let (total_messages, messages_vec) =
            thread.read_with(cx, |t, _| (t.messages().len(), t.messages().to_vec()));

        if input.start >= total_messages {
            let mut out = String::new();
            out.push_str("# Conversation History\n\n");
            out.push_str(&format!(
                "No messages found starting from index {} (total messages: {})\n",
                input.start, total_messages
            ));
            return Task::ready(Ok(out));
        }

        let end_index = (input.start + input.limit).min(total_messages);
        let slice = &messages_vec[input.start..end_index];

        let mut output = String::new();
        output.push_str("# Conversation History\n\n");

        // Summary JSON block
        output.push_str("```json\n");
        output.push_str(
            &serde_json::to_string_pretty(&serde_json::json!({
                "total_messages": total_messages,
                "showing_range": format!("{}..{}", input.start, end_index),
                "messages_shown": end_index - input.start,
            }))
            .unwrap_or_default(),
        );
        output.push_str("\n```\n\n");

        // Table header
        output.push_str("| Idx | Role | Chars | Preview |\n");
        output.push_str("|-----|------|-------|---------|\n");

        for (offset, message) in slice.iter().enumerate() {
            let idx = input.start + offset;
            let role = format!("{:?}", message.role());
            let markdown = message.to_markdown();
            let full: &str = markdown.as_ref();
            // Use character counts and char-based slicing to avoid slicing at invalid UTF-8 byte boundaries.
            let char_count = full.chars().count();
            let preview = if char_count <= input.max_chars_per_message {
                full.to_string()
            } else {
                full.chars().take(input.max_chars_per_message).collect::<String>()
            };
            let mut preview = preview.replace('|', "\\|").replace('\n', " ");
            if char_count > input.max_chars_per_message {
                preview.push_str("...");
            }
            output.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                idx, role, char_count, preview
            ));
        }

        if input.include_full_markdown {
            output.push_str("\n## Full Message Content\n\n");
            for (offset, message) in slice.iter().enumerate() {
                let idx = input.start + offset;
                let role = format!("{:?}", message.role());
                let markdown = message.to_markdown();
                output.push_str(&format!("### Message {} ({})\n\n", idx, role));
                output.push_str(markdown.as_ref());
                output.push_str("\n\n");
            }
        }

        Task::ready(Ok(output))
    }
}
