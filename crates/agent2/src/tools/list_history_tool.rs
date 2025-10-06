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

pub struct ListHistoryTool;

impl ListHistoryTool {
    pub fn new() -> Self {
        Self
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
        _cx: &mut App,
    ) -> Task<Result<Self::Output>> {
        // Clamp values to valid ranges
        input.limit = input.limit.clamp(1, 500);
        input.max_chars_per_message = input.max_chars_per_message.clamp(16, 4096);

        // For now, return a placeholder response indicating the feature needs thread context
        let mut output = String::new();
        output.push_str("# Conversation History\n\n");
        output.push_str("**Note:** This tool requires access to the thread's message history.\n");
        output.push_str("The full implementation is pending thread context integration.\n\n");

        output.push_str(&format!("Requested parameters:\n"));
        output.push_str(&format!("- Start index: {}\n", input.start));
        output.push_str(&format!("- Limit: {}\n", input.limit));
        output.push_str(&format!(
            "- Max chars per message: {}\n",
            input.max_chars_per_message
        ));
        output.push_str(&format!(
            "- Include full markdown: {}\n",
            input.include_full_markdown
        ));

        Task::ready(Ok(output))
    }
}
