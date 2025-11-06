use std::sync::Arc;

use anyhow::{Result, anyhow};
use gpui::{App, SharedString, Task};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{AgentTool, ToolCallEventStream};

/// Reuse the rich chat history adapter & operation types from assistant_tools.
use assistant_tools::{ChatHistoryOperation, chat_history_adapter};
use chat_history::{MessageRole, RetrievalMode};
use chat_history_tools::ChatHistoryToolApi;
use serde_json::json;

/// Agent-side input wrapper (mirrors adapter input; kept separate to allow future
/// agent-specific extensions like thread-scoped overrides).
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ChatHistoryAgentToolInput {
    pub operation: ChatHistoryOperation,
}

/// Output type: markdown string embedding any structured JSON blocks.
type ChatHistoryAgentToolOutput = String;

/// Agent tool bridging the assistant_tools ChatHistory adapter into the agent2
/// internal tool system (enabling direct LLM invocation with mutations).
pub struct ChatHistoryAgentTool;

impl ChatHistoryAgentTool {
    pub fn new() -> Self {
        Self
    }
}

impl AgentTool for ChatHistoryAgentTool {
    type Input = ChatHistoryAgentToolInput;
    type Output = ChatHistoryAgentToolOutput;

    fn name() -> &'static str {
        "ctx_chat_history"
    }

    fn kind() -> agent_client_protocol::ToolKind {
        // Read classification (mutations still allowed; current ToolKind enum
        // lacks a granular write + read hybrid).
        agent_client_protocol::ToolKind::Read
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        match input {
            Ok(i) => {
                let label = match i.operation {
                    ChatHistoryOperation::Append { .. } => "Append chat msg",
                    ChatHistoryOperation::Search { .. } => "Search chats",
                    ChatHistoryOperation::Answer { .. } => "Answer from chats",
                    ChatHistoryOperation::Similar { .. } => "Similar chats",
                    ChatHistoryOperation::List { .. } => "List chats",
                    ChatHistoryOperation::Get { .. } => "Get chat",
                    ChatHistoryOperation::CreateChat { .. } => "Create chat",
                    ChatHistoryOperation::DeleteChat { .. } => "Delete chat",
                    ChatHistoryOperation::Reembed { .. } => "Reembed chats",
                    ChatHistoryOperation::UpdateMetadata { .. } => "Update chat metadata",
                    ChatHistoryOperation::ConfigGet {} => "Get chat config",
                    ChatHistoryOperation::ConfigSet { .. } => "Set chat config",
                };
                label.into()
            }
            Err(_) => "Chat history".into(),
        }
    }

    fn run(
        self: Arc<Self>,
        input: Self::Input,
        _event_stream: ToolCallEventStream,
        _cx: &mut App,
    ) -> Task<Result<Self::Output>> {
        let op = input.operation;
        let adapter = match chat_history_adapter() {
            Some(a) => a,
            None => return Task::ready(Err(anyhow!("chat history adapter not installed"))),
        };

        // Helper to serialize adapter JSON result into a markdown fenced block.
        fn md_json_block(title: &str, value: &Value) -> String {
            let mut out = String::new();
            out.push_str("# ");
            out.push_str(title);
            out.push_str("\n\n```json\n");
            out.push_str(&serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".into()));
            out.push_str("\n```\n");
            out
        }

        _cx.spawn({
            let adapter = adapter.clone();
            async move |_cx| {
                let mk = |label: &str, raw: String| -> Result<String> {
                    let value: Value = serde_json::from_str(&raw)
                        .unwrap_or_else(|_| json!({"ok": false, "error": "invalid json"}));
                    Ok(md_json_block(label, &value))
                };

                match op {
                    ChatHistoryOperation::Append {
                        chat_id,
                        project_id,
                        title,
                        role,
                        content,
                    } => {
                        let payload = json!({
                            "chat_id": chat_id,
                            "project_id": project_id,
                            "title": title,
                            "role": role.unwrap_or(MessageRole::User),
                            "content": content
                        })
                        .to_string();
                        mk("Chat Append", adapter.chat_append(&payload).await)
                    }
                    ChatHistoryOperation::Search {
                        query,
                        project_id,
                        chat_id,
                        top_k,
                        mode,
                        alpha,
                    } => {
                        let payload = json!({
                            "query": query,
                            "project_id": project_id,
                            "chat_id": chat_id,
                            "top_k": top_k,
                            "mode": mode.unwrap_or(RetrievalMode::Hybrid),
                            "alpha": alpha
                        })
                        .to_string();
                        mk("Chat Search Results", adapter.chat_search(&payload).await)
                    }
                    ChatHistoryOperation::Answer {
                        question,
                        project_id,
                        chat_id,
                        top_k,
                        mode,
                        alpha,
                    } => {
                        let payload = json!({
                            "question": question,
                            "project_id": project_id,
                            "chat_id": chat_id,
                            "top_k": top_k,
                            "mode": mode.unwrap_or(RetrievalMode::Hybrid),
                            "alpha": alpha
                        })
                        .to_string();
                        mk("Chat Answer", adapter.chat_answer(&payload).await)
                    }
                    ChatHistoryOperation::Similar {
                        chat_id,
                        n,
                        project_scoped,
                    } => {
                        let payload = json!({
                            "chat_id": chat_id,
                            "n": n,
                            "project_scoped": project_scoped
                        })
                        .to_string();
                        mk("Similar Chats", adapter.chat_similar(&payload).await)
                    }
                    ChatHistoryOperation::List {
                        project_id,
                        limit,
                        offset,
                    } => {
                        let payload = json!({
                            "project_id": project_id,
                            "limit": limit,
                            "offset": offset
                        })
                        .to_string();
                        mk("Chat List", adapter.chat_list(&payload).await)
                    }
                    ChatHistoryOperation::Get { chat_id } => {
                        let payload = json!({ "chat_id": chat_id }).to_string();
                        mk("Chat", adapter.chat_get(&payload).await)
                    }
                    ChatHistoryOperation::CreateChat { project_id, title } => {
                        let payload = json!({
                            "project_id": project_id,
                            "title": title
                        })
                        .to_string();
                        mk("Chat Created", adapter.chat_create(&payload).await)
                    }
                    ChatHistoryOperation::DeleteChat { chat_id } => {
                        let payload = json!({ "chat_id": chat_id }).to_string();
                        mk("Chat Deleted", adapter.chat_delete(&payload).await)
                    }
                    ChatHistoryOperation::Reembed { chat_id } => {
                        let payload = json!({ "chat_id": chat_id }).to_string();
                        mk("Reembed Status", adapter.chat_reembed(&payload).await)
                    }
                    ChatHistoryOperation::UpdateMetadata {
                        chat_id,
                        title,
                        summary,
                        tags_add,
                        tags_remove,
                        archived,
                        pinned,
                    } => {
                        let payload = json!({
                            "chat_id": chat_id,
                            "title": title,
                            "summary": summary,
                            "tags_add": tags_add,
                            "tags_remove": tags_remove,
                            "archived": archived,
                            "pinned": pinned
                        })
                        .to_string();
                        mk(
                            "Metadata Update",
                            adapter.chat_update_metadata(&payload).await,
                        )
                    }
                    ChatHistoryOperation::ConfigGet {} => {
                        mk("Chat Config", adapter.chat_config_get().await)
                    }
                    ChatHistoryOperation::ConfigSet {
                        embedding_model,
                        hybrid_alpha,
                        similar_chats_k,
                        summary_refresh_chars,
                        summary_delta_chars,
                        rag_top_k,
                        auto_tag,
                        default_retrieval_mode,
                    } => {
                        let payload = json!({
                            "embedding_model": embedding_model,
                            "hybrid_alpha": hybrid_alpha,
                            "similar_chats_k": similar_chats_k,
                            "summary_refresh_chars": summary_refresh_chars,
                            "summary_delta_chars": summary_delta_chars,
                            "rag_top_k": rag_top_k,
                            "auto_tag": auto_tag,
                            "default_retrieval_mode": default_retrieval_mode
                        })
                        .to_string();
                        mk("Chat Config Set", adapter.chat_config_set(&payload).await)
                    }
                }
            }
        })
    }

    // may_perform_edits removed; AgentTool trait does not define this method
}

#[cfg(test)]
mod tests {}
