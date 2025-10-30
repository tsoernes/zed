use std::sync::Arc;

use anyhow::Result;
use gpui::{App, SharedString, Task};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AgentTool, ToolCallEventStream};

/// Input schema for the `chat_history` tool.
///
/// This stub implementation is intentionally minimal. It exposes two read‑only
/// operations that allow the agent to (in the future) inspect or snapshot
/// conversation‑level metadata without requiring an external multiplexer.
///
/// Future extensions could include:
/// * Append / tag messages
/// * Persist named snapshots
/// * Diff snapshots
/// * Selective export
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChatHistoryAction {
    /// Return a compact markdown overview (placeholder in this stub).
    Overview,
    /// Return a JSON metadata snapshot (placeholder in this stub).
    Snapshot,
}

/// Top–level input wrapper.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ChatHistoryToolInput {
    pub action: ChatHistoryAction,
}

/// Tool output (markdown).
type ChatHistoryToolOutput = String;

/// Unit struct so it can be registered with `add_tool(ChatHistoryAgentTool)`.
/// (Thread access can be added later if mutation or deep inspection is required.)
pub struct ChatHistoryAgentTool;

impl ChatHistoryAgentTool {
    pub fn new() -> Self {
        Self
    }
}

impl AgentTool for ChatHistoryAgentTool {
    type Input = ChatHistoryToolInput;
    type Output = ChatHistoryToolOutput;

    fn name() -> &'static str {
        "chat_history"
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
            Ok(i) => match i.action {
                ChatHistoryAction::Overview => "Chat history overview".into(),
                ChatHistoryAction::Snapshot => "Chat history snapshot".into(),
            },
            Err(_) => "Chat history".into(),
        }
    }

    fn run(
        self: Arc<Self>,
        input: Self::Input,
        _event_stream: ToolCallEventStream,
        _cx: &mut App,
    ) -> Task<Result<Self::Output>> {
        let markdown = match input.action {
            ChatHistoryAction::Overview => {
                "# Chat History Overview (Stub)\n\n\
                 This is a placeholder implementation of `chat_history`.\n\
                 It currently provides no real conversation integration.\n\
                 Future versions will surface structured summaries of the\n\
                 active / archived messages and tagging metadata.\n"
                    .to_string()
            }
            ChatHistoryAction::Snapshot => {
                "# Chat History Snapshot (Stub)\n\n\
                 ```json\n\
                 {\n  \"status\": \"unimplemented\",\n  \"version\": 1,\n  \"notes\": \"This tool will expose structured chat metadata.\" \n}\n\
                 ```\n"
                .to_string()
            }
        };
        Task::ready(Ok(markdown))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;

    #[test]
    fn initial_title_matches_action() {
        let tool = ChatHistoryAgentTool::new();
        let title_overview = tool.initial_title(
            Ok(ChatHistoryToolInput {
                action: ChatHistoryAction::Overview,
            }),
            &mut gpui::App::test(),
        );
        let title_snapshot = tool.initial_title(
            Ok(ChatHistoryToolInput {
                action: ChatHistoryAction::Snapshot,
            }),
            &mut gpui::App::test(),
        );
        assert!(title_overview.contains("overview"));
        assert!(title_snapshot.contains("snapshot"));

        let title_err = tool.initial_title(
            Err(serde_json::json!({"bad":"data"})),
            &mut gpui::App::test(),
        );
        assert_eq!(title_err.as_str(), "Chat history");
    }

    #[test]
    fn run_produces_markdown() {
        let tool = Arc::new(ChatHistoryAgentTool::new());
        let task = tool.run(
            ChatHistoryToolInput {
                action: ChatHistoryAction::Overview,
            },
            ToolCallEventStream::noop("chat_history_test"),
            &mut gpui::App::test(),
        );
        let out = futures::executor::block_on(task).unwrap();
        assert!(out.starts_with("# Chat History Overview"));
    }
}
