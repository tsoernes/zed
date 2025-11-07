use std::sync::Arc;

use anyhow::{Result, anyhow};
use gpui::{App, SharedString, Task};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{AgentTool, ToolCallEventStream};
use assistant_tools::chat_history_adapter;
use chat_history_tools::ChatHistoryToolApi;

/// Update metadata and organization details for a chat conversation.
///
/// This tool allows you to modify a chat's organizational information:
/// - Title: Update the conversation's display name
/// - Summary: Add or update a summary of the chat's content
/// - Tags: Add or remove tags for categorization
/// - Archive status: Mark chats as archived to declutter your list
/// - Pin status: Pin important chats for quick access
///
/// Use this to:
/// - Organize your chat history with descriptive titles
/// - Add tags for easy filtering and categorization
/// - Summarize long conversations for future reference
/// - Archive completed or outdated discussions
/// - Pin frequently referenced conversations
///
/// All fields are optional - provide only the fields you want to update.
/// Tags can be added or removed independently without affecting existing tags.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ChatUpdateInput {
    /// The unique identifier of the chat to update.
    ///
    /// You can obtain chat IDs from chat_list, chat_search, or chat_similar tools.
    pub chat_id: String,

    /// New title for the chat (optional).
    ///
    /// Use descriptive titles that help identify the conversation:
    /// - "Database optimization discussion - Dec 2024"
    /// - "Fix async error handling in API service"
    /// - "Architecture planning for auth system"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,

    /// Summary of the chat's content (optional).
    ///
    /// Add a brief summary to help recall what the conversation covered:
    /// - Key decisions made
    /// - Problems solved
    /// - Important conclusions or outcomes
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,

    /// Tags to add to this chat (optional).
    ///
    /// Tags help organize and filter conversations. Examples:
    /// - ["bug-fix", "database"]
    /// - ["architecture", "planning"]
    /// - ["completed", "production"]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags_add: Option<Vec<String>>,

    /// Tags to remove from this chat (optional).
    ///
    /// Remove tags that are no longer relevant. You can add and remove
    /// tags in the same operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags_remove: Option<Vec<String>>,

    /// Archive status (optional).
    ///
    /// Set to true to archive the chat (hide from main list but keep searchable).
    /// Set to false to unarchive and restore to main list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,

    /// Pin status (optional).
    ///
    /// Set to true to pin the chat to the top of your chat list.
    /// Set to false to unpin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,
}

pub struct ChatUpdateTool;

impl ChatUpdateTool {
    pub fn new() -> Self {
        Self
    }
}

impl AgentTool for ChatUpdateTool {
    type Input = ChatUpdateInput;
    type Output = String;

    fn name() -> &'static str {
        "chat_update"
    }

    fn description(&self) -> SharedString {
        "Update a chat's metadata including title, summary, tags, archive status, and pin status. \
         Use this to organize your chat history with descriptive titles, categorize with tags, \
         add summaries for future reference, and manage which chats are archived or pinned."
            .into()
    }

    fn kind() -> agent_client_protocol::ToolKind {
        agent_client_protocol::ToolKind::Edit
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        match input {
            Ok(i) => format!("Update chat {}", i.chat_id).into(),
            Err(_) => "Update chat metadata".into(),
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
                    "title": input.title,
                    "summary": input.summary,
                    "tags_add": input.tags_add,
                    "tags_remove": input.tags_remove,
                    "archived": input.archived,
                    "pinned": input.pinned
                })
                .to_string();

                let raw_result = adapter.chat_update_metadata(&payload).await;
                let value: Value = serde_json::from_str(&raw_result).unwrap_or_else(|_| {
                    json!({"ok": false, "error": "Failed to parse update result"})
                });

                let mut output = String::from("# Chat Metadata Updated\n\n");
                output.push_str("```json\n");
                output.push_str(&serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into()));
                output.push_str("\n```\n");

                Ok(output)
            }
        })
    }
}
