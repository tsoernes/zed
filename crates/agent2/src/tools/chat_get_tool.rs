use std::sync::Arc;

use anyhow::{Result, anyhow};
use gpui::{App, SharedString, Task};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{AgentTool, ToolCallEventStream};
use assistant_tools::chat_history_adapter;
use chat_history_tools::ChatHistoryToolApi;

/// Retrieve a specific chat conversation with its complete message history.
///
/// This tool fetches the full details of a specific chat, including:
/// - All messages in the conversation (with roles, content, timestamps)
/// - Chat metadata (title, creation date, last update)
/// - Associated project information
/// - Tags and organization details
///
/// Use this when you:
/// - Need to review a specific conversation in detail
/// - Want to see the complete message history of a chat
/// - Need context from a particular discussion
/// - Want to analyze the flow of a past conversation
///
/// The chat is identified by its unique chat_id, which you can obtain from
/// chat_list or chat_search tools.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ChatGetInput {
    /// The unique identifier of the chat to retrieve.
    ///
    /// You can obtain chat IDs from:
    /// - chat_list: Browse all available chats
    /// - chat_search: Find chats matching a query
    /// - chat_similar: Discover related conversations
    pub chat_id: String,
}

pub struct ChatGetTool;

impl ChatGetTool {
    pub fn new() -> Self {
        Self
    }
}

impl AgentTool for ChatGetTool {
    type Input = ChatGetInput;
    type Output = String;

    fn name() -> &'static str {
        "chat_get"
    }

    fn description(&self) -> SharedString {
        "Retrieve a specific chat conversation with its complete message history and metadata. \
         Use this to review past discussions in detail. Provide a chat_id to get the full \
         conversation including all messages, timestamps, and associated information."
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
            Ok(i) => format!("Get chat {}", i.chat_id).into(),
            Err(_) => "Get chat".into(),
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
                    "chat_id": input.chat_id
                })
                .to_string();

                let raw_result = adapter.chat_get(&payload).await;
                let value: Value = serde_json::from_str(&raw_result).unwrap_or_else(|_| {
                    json!({"ok": false, "error": "Failed to parse chat data"})
                });

                let mut output = String::from("# Chat Details\n\n");
                output.push_str("```json\n");
                output.push_str(&serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into()));
                output.push_str("\n```\n");

                Ok(output)
            }
        })
    }
}
