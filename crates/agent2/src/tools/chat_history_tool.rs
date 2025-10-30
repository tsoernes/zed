use std::sync::Arc;

use anyhow::{Result, anyhow};
use gpui::{App, SharedString, Task};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{AgentTool, ToolCallEventStream};

/// Reuse the rich chat history adapter & operation types from assistant_tools.
use assistant_tools::context_management::chat_history_tool::{
    ChatHistoryOperation, ChatHistoryToolInput as AdapterChatHistoryToolInput, chat_history_adapter,
};
use chat_history::{MessageRole, RetrievalMode};

/// Agent-side input wrapper (mirrors adapter input; kept separate to allow future
/// agent-specific extensions like thread-scoped overrides).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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

    fn is_mutating(op: &ChatHistoryOperation) -> bool {
        matches!(
            op,
            ChatHistoryOperation::Append { .. }
                | ChatHistoryOperation::Reembed { .. }
                | ChatHistoryOperation::UpdateMetadata { .. }
                | ChatHistoryOperation::ConfigSet { .. }
        )
    }
}

impl AgentTool for ChatHistoryAgentTool {
    type Input = ChatHistoryAgentToolInput;
    type Output = ChatHistoryAgentToolOutput;

    fn name() -> &'static str {
        "chat_history"
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
                    ChatHistoryOperation::Reembed { .. } => "Reembed chats",
                    ChatHistoryOperation::UpdateMetadata { .. } => "Update chat metadata",
                    ChatHistoryOperation::ConfigGet => "Get chat config",
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

        let task_result: Result<String> = (|| match op {
            ChatHistoryOperation::Append {
                chat_id,
                project_id,
                title,
                role,
                content,
            } => {
                let role = role.unwrap_or(MessageRole::User);
                let resp = adapter.append(chat_id, project_id, title, role, content)?;
                Ok(md_json_block("Chat Append", &resp))
            }
            ChatHistoryOperation::Search {
                query,
                project_id,
                chat_id,
                top_k,
                mode,
                alpha,
            } => {
                let resp = adapter.search(
                    query,
                    project_id,
                    chat_id,
                    top_k,
                    mode.unwrap_or(RetrievalMode::Hybrid),
                    alpha,
                )?;
                Ok(md_json_block("Chat Search Results", &resp))
            }
            ChatHistoryOperation::Answer {
                question,
                project_id,
                chat_id,
                top_k,
                mode,
                alpha,
            } => {
                let resp = adapter.answer(
                    question,
                    project_id,
                    chat_id,
                    top_k,
                    mode.unwrap_or(RetrievalMode::Hybrid),
                    alpha,
                )?;
                Ok(md_json_block("Chat Answer", &resp))
            }
            ChatHistoryOperation::Similar {
                chat_id,
                n,
                project_scoped,
            } => {
                let resp = adapter.similar(chat_id, n, project_scoped)?;
                Ok(md_json_block("Similar Chats", &resp))
            }
            ChatHistoryOperation::List {
                project_id,
                limit,
                offset,
            } => {
                let resp = adapter.list(project_id, limit, offset)?;
                Ok(md_json_block("Chat List", &resp))
            }
            ChatHistoryOperation::Get { chat_id } => {
                let resp = adapter.get(chat_id)?;
                Ok(md_json_block("Chat", &resp))
            }
            ChatHistoryOperation::Reembed { chat_id } => {
                let resp = adapter.reembed(chat_id)?;
                Ok(md_json_block("Reembed Status", &resp))
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
                let resp = adapter.update_metadata(
                    chat_id,
                    title,
                    summary,
                    tags_add.unwrap_or_default(),
                    tags_remove.unwrap_or_default(),
                    archived,
                    pinned,
                )?;
                Ok(md_json_block("Metadata Update", &resp))
            }
            ChatHistoryOperation::ConfigGet => {
                let resp = adapter.config_get()?;
                Ok(md_json_block("Chat Config", &resp))
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
                let resp = adapter.config_set(
                    embedding_model,
                    hybrid_alpha,
                    similar_chats_k,
                    summary_refresh_chars,
                    summary_delta_chars,
                    rag_top_k,
                    auto_tag,
                    default_retrieval_mode,
                )?;
                Ok(md_json_block("Chat Config Set", &resp))
            }
        })();

        Task::ready(task_result)
    }

    fn may_perform_edits(&self) -> bool {
        // Indicate potential state mutation for operations flagged mutating.
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::App;

    #[test]
    fn title_mapping() {
        let tool = ChatHistoryAgentTool::new();
        let t = tool.initial_title(
            Ok(ChatHistoryAgentToolInput {
                operation: ChatHistoryOperation::List {
                    project_id: None,
                    limit: Some(5),
                    offset: None,
                },
            }),
            &mut App::test(),
        );
        assert!(t.contains("List"));
    }

    #[test]
    fn mutating_flag() {
        let tool = ChatHistoryAgentTool::new();
        assert!(tool.may_perform_edits());
    }
}
