use std::sync::Arc;

use anyhow::{Result, anyhow};
use gpui::{App, SharedString, Task};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{AgentTool, ToolCallEventStream};
use assistant_tools::chat_history_adapter;
use chat_history::RetrievalMode;
use chat_history_tools::ChatHistoryToolApi;

/// Search across all stored chat conversations using keyword and semantic search.
///
/// This tool performs hybrid search (combining BM25 keyword matching with semantic embeddings)
/// to find relevant messages across your entire chat history. Perfect for:
/// - Finding past discussions about specific topics
/// - Locating where you solved similar problems before
/// - Discovering relevant context from previous conversations
/// - Searching for specific code examples or solutions discussed previously
///
/// The search uses a hybrid approach by default, combining:
/// - BM25: Fast keyword/phrase matching
/// - Embeddings: Semantic similarity for conceptual matches
///
/// Results are ranked by relevance and include context snippets from matching messages.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ChatSearchInput {
    /// The search query text. Use natural language or keywords.
    ///
    /// Examples:
    /// - "error handling in async rust"
    /// - "how to configure database connections"
    /// - "fix memory leak issue"
    pub query: String,

    /// Optional project identifier to limit search to a specific project.
    /// If omitted, searches across all projects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,

    /// Optional chat identifier to search within a single conversation.
    /// Useful for finding specific messages in a known chat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,

    /// Maximum number of results to return (default: 10).
    /// Higher values provide more context but may be overwhelming.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<usize>,

    /// Search mode: "hybrid" (default), "bm25" (keyword-only), or "embedding" (semantic-only).
    ///
    /// - "hybrid": Best for most queries - combines keyword and semantic matching
    /// - "bm25": Use when searching for exact phrases or technical terms
    /// - "embedding": Use for conceptual searches when exact wording varies
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,

    /// Fusion weight for hybrid search (0.0 to 1.0, default: 0.55).
    /// - 0.0 = pure keyword (BM25 only)
    /// - 1.0 = pure semantic (embeddings only)
    /// - 0.55 = balanced hybrid (recommended)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alpha: Option<f32>,
}

pub struct ChatSearchTool;

impl ChatSearchTool {
    pub fn new() -> Self {
        Self
    }
}

impl AgentTool for ChatSearchTool {
    type Input = ChatSearchInput;
    type Output = String;

    fn name() -> &'static str {
        "chat_search"
    }

    fn description(&self) -> SharedString {
        "Search through your chat history using keywords and semantic similarity. \
         Finds relevant messages across all conversations. Use this to locate past \
         discussions, solutions, or context about specific topics."
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
            Ok(i) => format!("Search chats: {}", i.query).into(),
            Err(_) => "Search chats".into(),
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
                let mode = input.mode.as_deref().and_then(|m| match m {
                    "bm25" => Some(RetrievalMode::Bm25),
                    "embedding" => Some(RetrievalMode::Embedding),
                    "hybrid" => Some(RetrievalMode::Hybrid),
                    _ => None,
                });

                let payload = json!({
                    "query": input.query,
                    "project_id": input.project_id,
                    "chat_id": input.chat_id,
                    "top_k": input.top_k,
                    "mode": mode.unwrap_or(RetrievalMode::Hybrid),
                    "alpha": input.alpha
                })
                .to_string();

                let raw_result = adapter.chat_search(&payload).await;
                let value: Value = serde_json::from_str(&raw_result).unwrap_or_else(|_| {
                    json!({"ok": false, "error": "Failed to parse search results"})
                });

                let mut output = String::from("# Chat Search Results\n\n");
                output.push_str("```json\n");
                output.push_str(&serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into()));
                output.push_str("\n```\n");

                Ok(output)
            }
        })
    }
}
