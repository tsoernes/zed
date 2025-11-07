use std::sync::Arc;

use anyhow::{Result, anyhow};
use gpui::{App, SharedString, Task};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{AgentTool, ToolCallEventStream};
use assistant_tools::chat_history_adapter;
use chat_history_tools::ChatHistoryToolApi;

/// List stored chat conversations with metadata and pagination support.
///
/// This tool retrieves a list of your saved chat conversations, showing:
/// - Chat titles and IDs
/// - Creation and last update timestamps
/// - Project associations
/// - Tags and metadata
/// - Message counts
///
/// Use this to:
/// - Browse your conversation history
/// - Find specific chats by title or topic
/// - See recent conversations
/// - Discover what information is available in your chat history
///
/// Results are paginated for efficient browsing of large chat histories.
/// Use limit and offset parameters to navigate through pages of results.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ChatListInput {
    /// Optional project identifier to filter chats by project.
    /// If omitted, lists chats from all projects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,

    /// Maximum number of chats to return per page (default: 20).
    /// Use smaller values for quick overviews, larger for comprehensive lists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,

    /// Number of chats to skip for pagination (default: 0).
    /// Use this to navigate to subsequent pages:
    /// - Page 1: offset = 0
    /// - Page 2: offset = limit
    /// - Page 3: offset = limit * 2
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<usize>,
}

pub struct ChatListTool;

impl ChatListTool {
    pub fn new() -> Self {
        Self
    }
}

impl AgentTool for ChatListTool {
    type Input = ChatListInput;
    type Output = String;

    fn name() -> &'static str {
        "chat_list"
    }

    fn description(&self) -> SharedString {
        "List your stored chat conversations with metadata, titles, and timestamps. \
         Use this to browse your conversation history, find specific chats, or see \
         what information is available. Supports filtering by project and pagination."
            .into()
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
            Ok(i) => {
                if let Some(project) = &i.project_id {
                    format!("List chats in {}", project).into()
                } else {
                    "List all chats".into()
                }
            }
            Err(_) => "List chats".into(),
        }
    }

    fn run(
        self: Arc<Self>,
        input: Self::Input,
        _event_stream: ToolCallEventStream,
        _cx: &mut App,
    ) -> Task<Result<Self::Output>> {
        let adapter = match chat_history_adapter() {
            Some(a) => a,
            None => {
                return Task::ready(Err(anyhow!(
                    "Chat history adapter not installed. The chat history system may not be initialized."
                )))
            }
        };

        _cx.spawn({
            let adapter = adapter.clone();
            async move |_cx| {
                let payload = json!({
                    "project_id": input.project_id,
                    "limit": input.limit,
                    "offset": input.offset
                })
                .to_string();

                let raw_result = adapter.chat_list(&payload).await;
                let value: Value = serde_json::from_str(&raw_result).unwrap_or_else(|_| {
                    json!({"ok": false, "error": "Failed to parse chat list"})
                });

                let mut output = String::from("# Chat List\n\n");
                output.push_str("```json\n");
                output.push_str(&serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into()));
                output.push_str("\n```\n");

                Ok(output)
            }
        })
    }
}
