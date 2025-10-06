use crate::{AgentTool, ToolCallEventStream};
use agent_client_protocol::ToolKind;
use anyhow::Result;
use gpui::{App, SharedString, Task};
use log::debug;
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Manages conversation memory archival and retrieval for context window management.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryOperation {
    /// Archive a contiguous range of messages
    Store,
    /// Retrieve full original serialized content for inspection
    Load,
    /// Scan current thread for memory placeholders
    List,
    /// Insert archived messages back into the conversation
    Restore,
    /// Remove archived handles that no longer have placeholders
    Prune,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct MemoryToolInput {
    /// The operation to perform
    operation: MemoryOperation,

    /// Required for store: inclusive starting message index
    #[serde(skip_serializing_if = "Option::is_none")]
    start_index: Option<usize>,

    /// Required for store: inclusive ending message index
    #[serde(skip_serializing_if = "Option::is_none")]
    end_index: Option<usize>,

    /// Required for load/restore: memory handle
    #[serde(skip_serializing_if = "Option::is_none")]
    memory_handle: Option<String>,

    /// Optional for store: user-defined summary
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,

    /// For store: if true and summary omitted, generate heuristic summary
    #[serde(default)]
    auto: bool,

    /// Maximum characters to include in generated previews (clamped internally)
    #[serde(default = "default_preview_chars")]
    max_preview_chars: usize,

    /// Index at which to insert restored messages (restore operation)
    #[serde(skip_serializing_if = "Option::is_none")]
    restore_insert_index: Option<usize>,

    /// Whether to remove the placeholder after restore
    #[serde(default)]
    remove_placeholder: bool,

    /// If provided, replaces the placeholder with this text instead of removing it
    #[serde(skip_serializing_if = "Option::is_none")]
    replace_placeholder_with: Option<String>,
}

fn default_preview_chars() -> usize {
    160
}

pub struct MemoryTool;

impl MemoryTool {
    pub fn new() -> Self {
        Self
    }
}

impl AgentTool for MemoryTool {
    type Input = MemoryToolInput;
    type Output = String;

    fn name() -> &'static str {
        // Log the schema property keys once to help diagnose external (Claude) 400 errors
        {
            let schema = schema_for!(MemoryToolInput);
            if let Some(obj) = schema.schema.object {
                let keys: Vec<_> = obj.properties.keys().cloned().collect();
                debug!("agent2::MemoryToolInput schema properties: {:?}", keys);
            }
        }
        "memory"
    }

    fn kind() -> ToolKind {
        ToolKind::Other
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        if let Ok(input) = input {
            match input.operation {
                MemoryOperation::Store => {
                    if let (Some(start), Some(end)) = (input.start_index, input.end_index) {
                        format!("Archive messages {}-{}", start, end).into()
                    } else {
                        "Archive messages".into()
                    }
                }
                MemoryOperation::Load => {
                    if let Some(handle) = input.memory_handle {
                        format!("Load memory {}", handle).into()
                    } else {
                        "Load memory".into()
                    }
                }
                MemoryOperation::List => "List archived memories".into(),
                MemoryOperation::Restore => "Restore archived messages".into(),
                MemoryOperation::Prune => "Prune unused memories".into(),
            }
        } else {
            "Memory operation".into()
        }
    }

    fn run(
        self: Arc<Self>,
        input: Self::Input,
        _event_stream: ToolCallEventStream,
        _cx: &mut App,
    ) -> Task<Result<Self::Output>> {
        // For now, return a placeholder response indicating the feature needs thread context
        let mut output = String::new();

        match input.operation {
            MemoryOperation::Store => {
                output.push_str("# Memory Archive (Store)\n\n");
                output.push_str(
                    "**Note:** This tool requires access to the thread's message history.\n",
                );
                output
                    .push_str("The full implementation is pending thread context integration.\n\n");

                if let (Some(start), Some(end)) = (input.start_index, input.end_index) {
                    output.push_str(&format!("Would archive messages {} to {}\n", start, end));
                }
                if let Some(summary) = input.summary {
                    output.push_str(&format!("Summary: {}\n", summary));
                }
            }
            MemoryOperation::Load => {
                output.push_str("# Memory Load\n\n");
                output.push_str("**Note:** This tool requires persistent memory storage.\n");
                output.push_str("The full implementation is pending.\n\n");

                if let Some(handle) = input.memory_handle {
                    output.push_str(&format!("Would load memory: {}\n", handle));
                }
            }
            MemoryOperation::List => {
                output.push_str("# Archived Memories\n\n");
                output.push_str("No memories currently stored.\n");
                output.push_str("(Memory persistence pending implementation)\n");
            }
            MemoryOperation::Restore => {
                output.push_str("# Memory Restore\n\n");
                output.push_str("**Note:** This tool requires thread message manipulation.\n");
                output.push_str("The full implementation is pending.\n\n");

                if let Some(handle) = input.memory_handle {
                    output.push_str(&format!("Would restore memory: {}\n", handle));
                }
            }
            MemoryOperation::Prune => {
                output.push_str("# Memory Prune\n\n");
                output.push_str("No memories to prune.\n");
            }
        }

        Task::ready(Ok(output))
    }
}
