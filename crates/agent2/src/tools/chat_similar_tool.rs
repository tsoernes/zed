use std::sync::Arc;

use anyhow::{Result, anyhow};
use gpui::{App, SharedString, Task};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{AgentTool, ToolCallEventStream};
use assistant_tools::chat_history_adapter;
use chat_history_tools::ChatHistoryToolApi;

/// Find conversations that are semantically similar to the current or a specified chat.
///
/// This tool uses embeddings to discover related conversations based on content similarity,
/// helping you find:
/// - Previous discussions about similar topics
/// - Related problems you've worked on before
/// - Conversations that might contain relevant context
/// - Past solutions to similar challenges
///
/// By default, it finds chats similar to your CURRENT conversation (no chat_id needed).
/// This is useful for discovering related discussions while you work.
///
/// The similarity is based on semantic content, not just keywords, so it can find
/// conceptually related conversations even if they use different terminology.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ChatSimilarInput {
    /// Optional chat ID to find similar conversations to.
    /// If omitted, uses the current conversation automatically.
    ///
    /// Leave this empty to find chats similar to what you're working on right now.
    /// Provide a chat_id to find conversations similar to a specific past chat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,

    /// Number of similar conversations to return (default: 10).
    /// Increasing this gives you more options but may include less relevant results.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n: Option<usize>,

    /// Whether to limit results to the current project (default: true).
    /// Set to false to search across all projects for similar conversations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_scoped: Option<bool>,
}

pub struct ChatSimilarTool;

impl ChatSimilarTool {
    pub fn new() -> Self {
        Self
    }
}

impl AgentTool for ChatSimilarTool {
    type Input = ChatSimilarInput;
    type Output = String;

    fn name() -> &'static str {
        "chat_similar"
    }

    fn description(&self) -> SharedString {
        "Find conversations semantically similar to the current chat or a specified conversation. \
         Discovers related discussions and past solutions to similar problems. By default finds \
         chats similar to your current conversation - no chat_id needed."
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
                if let Some(id) = &i.chat_id {
                    format!("Similar chats to {}", id).into()
                } else {
                    "Similar chats to current conversation".into()
                }
            }
            Err(_) => "Similar chats".into(),
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
                    "chat_id": input.chat_id,
                    "n": input.n,
                    "project_scoped": input.project_scoped
                })
                .to_string();

                let raw_result = adapter.chat_similar(&payload).await;
                let value: Value = serde_json::from_str(&raw_result).unwrap_or_else(|_| {
                    json!({"ok": false, "error": "Failed to parse similar chats results"})
                });

                let mut output = String::from("# Similar Conversations\n\n");
                output.push_str("```json\n");
                output.push_str(&serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into()));
                output.push_str("\n```\n");

                Ok(output)
            }
        })
    }
}
