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

/// Answer questions using Retrieval-Augmented Generation (RAG) over your chat history.
///
/// This tool searches your chat history for relevant context and generates an answer
/// with citations to specific conversations. It's perfect for:
/// - "How did I solve X problem before?"
/// - "What approach did we decide on for Y?"
/// - "Where did I discuss Z topic?"
/// - Recalling past decisions, solutions, or discussions
///
/// The tool performs hybrid search to find relevant messages, then synthesizes an
/// answer with inline citations showing which conversations the information came from.
/// This helps you leverage your accumulated knowledge and past problem-solving.
///
/// Unlike chat_search (which just finds messages), this tool actually answers your
/// question by combining information from multiple sources in your history.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ChatAnswerInput {
    /// The question to answer using your chat history.
    ///
    /// Examples:
    /// - "How did I fix the database connection timeout issue?"
    /// - "What was the recommended approach for handling async errors?"
    /// - "Where did I discuss the architecture for the API service?"
    pub question: String,

    /// Optional project identifier to limit search to a specific project.
    /// If omitted, searches across all projects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,

    /// Optional chat identifier to search within a single conversation.
    /// Useful when you know the answer is in a specific chat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,

    /// Maximum number of relevant passages to retrieve (default: 10).
    /// Higher values provide more context for the answer but may be slower.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<usize>,

    /// Retrieval mode: "hybrid" (default), "bm25" (keyword-only), or "embedding" (semantic-only).
    ///
    /// - "hybrid": Best for most questions - combines keyword and semantic search
    /// - "bm25": Use when the question contains specific technical terms
    /// - "embedding": Use for conceptual questions where phrasing varies
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,

    /// Fusion weight for hybrid retrieval (0.0 to 1.0, default: 0.55).
    /// - 0.0 = pure keyword matching
    /// - 1.0 = pure semantic similarity
    /// - 0.55 = balanced hybrid (recommended)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alpha: Option<f32>,
}

pub struct ChatAnswerTool;

impl ChatAnswerTool {
    pub fn new() -> Self {
        Self
    }
}

impl AgentTool for ChatAnswerTool {
    type Input = ChatAnswerInput;
    type Output = String;

    fn name() -> &'static str {
        "chat_answer"
    }

    fn description(&self) -> SharedString {
        "Answer questions by searching your chat history and synthesizing information with citations. \
         Use this to recall past solutions, decisions, or discussions. Returns an answer with references \
         to the specific conversations where the information was found."
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
            Ok(i) => format!("Answer: {}", i.question).into(),
            Err(_) => "Answer from chat history".into(),
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
                    "question": input.question,
                    "project_id": input.project_id,
                    "chat_id": input.chat_id,
                    "top_k": input.top_k,
                    "mode": mode.unwrap_or(RetrievalMode::Hybrid),
                    "alpha": input.alpha
                })
                .to_string();

                let raw_result = adapter.chat_answer(&payload).await;
                let value: Value = serde_json::from_str(&raw_result).unwrap_or_else(|_| {
                    json!({"ok": false, "error": "Failed to parse answer results"})
                });

                let mut output = String::from("# Answer from Chat History\n\n");
                output.push_str("```json\n");
                output.push_str(&serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into()));
                output.push_str("\n```\n");

                Ok(output)
            }
        })
    }
}
