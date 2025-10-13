use std::sync::Arc;

use anyhow::{Result, anyhow};
use gpui::{App, SharedString, Task, WeakEntity};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::thread::Thread;
use crate::{AgentTool, ToolCallEventStream};

/// Memory actions supported by the native agent `memory` tool.
/// Store uses a half-open range [start, end) (end is exclusive) to mirror the
/// assistant_tools MemoryTool semantics. Thread APIs require an inclusive end
/// index; conversion happens internally.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAction {
    /// List stored memory segments. Optional limit returns the most recent N
    /// (by id ascending, truncated from the front).
    List {
        #[serde(default)]
        limit: Option<usize>,
    },
    /// Aggregate statistics across all archived segments.
    Stats,
    /// Archive (store) a contiguous range of messages [start, end) (end exclusive).
    /// Replaces them with a single placeholder summary message.
    Store { start: usize, end: usize },
    /// Load a segment's metadata, optionally including original messages' markdown.
    Load {
        id: u64,
        #[serde(default)]
        include_messages: bool,
    },
    /// Restore a previously archived segment (reinsert messages, keep archive).
    Restore { id: u64 },
    /// Permanently prune (delete) the archived segment (and its placeholder if present).
    Prune { id: u64 },
}

/// Input schema for the native agent memory tool.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MemoryToolInput {
    pub operation: MemoryAction,
}

/// Tool output is markdown text describing the outcome and (when applicable) JSON data blocks.
type MemoryToolOutput = String;

/// Native agent memory tool (thread-backed).
///
/// This tool directly uses the active Thread entity to archive, inspect, restore,
/// and prune memory segments. It exists so the agent can manage long conversations
/// without relying on the external MCP proxy layer.
pub struct MemoryAgentTool {
    thread: WeakEntity<Thread>,
}

impl MemoryAgentTool {
    pub fn new(thread: WeakEntity<Thread>) -> Self {
        Self { thread }
    }
}

impl AgentTool for MemoryAgentTool {
    type Input = MemoryToolInput;
    type Output = MemoryToolOutput;

    fn name() -> &'static str {
        "memory"
    }

    fn kind() -> agent_client_protocol::ToolKind {
        // This tool may mutate conversation state (store / restore / prune), so classify as Write.
        agent_client_protocol::ToolKind::Write
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        match input {
            Ok(i) => match i.operation {
                MemoryAction::List { .. } => "List memories".into(),
                MemoryAction::Stats => "Memory stats".into(),
                MemoryAction::Store { start, end } => {
                    format!("Archive [{}..{})", start, end).into()
                }
                MemoryAction::Load { id, .. } => format!("Load memory {}", id).into(),
                MemoryAction::Restore { id } => format!("Restore memory {}", id).into(),
                MemoryAction::Prune { id } => format!("Prune memory {}", id).into(),
            },
            Err(_) => "Memory operation".into(),
        }
    }

    fn run(
        self: Arc<Self>,
        input: Self::Input,
        _event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output>> {
        let Some(thread) = self.thread.upgrade() else {
            return Task::ready(Ok(String::from("# Memory\n\nThread no longer exists.\n")));
        };

        // Execute synchronously on foreground (Thread APIs require &mut Context<Thread> for mutations).
        let result: Result<String> = match input.operation {
            MemoryAction::List { limit } => {
                let metas = thread.read_with(cx, |t, _| t.memory_segment_metas());
                let mut metas_sorted = metas;
                metas_sorted.sort_by_key(|m| m.0);
                let filtered = if let Some(l) = limit {
                    if metas_sorted.len() > l {
                        metas_sorted
                            .into_iter()
                            .rev()
                            .take(l)
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .collect::<Vec<_>>()
                    } else {
                        metas_sorted
                    }
                } else {
                    metas_sorted
                };
                let list_json: Vec<serde_json::Value> = filtered
                    .iter()
                    .map(|m| {
                        serde_json::json!({
                            "id": m.0,
                            "start": m.1,
                            "end": m.2,
                            "count": m.3,
                            "chars": m.4,
                            "placeholder_chars": m.5,
                            "token_savings_estimate": m.6,
                            "summary": m.7,
                            "stored_epoch_ms": m.8
                        })
                    })
                    .collect();
                let mut md = String::new();
                md.push_str("# Stored Memory Segments\n\n```json\n");
                md.push_str(&serde_json::to_string_pretty(&list_json)?);
                md.push_str("\n```\n");
                Ok(md)
            }
            MemoryAction::Stats => {
                let metas = thread.read_with(cx, |t, _| t.memory_segment_metas());
                let stats = serde_json::json!({
                    "segments": metas.len(),
                    "messages": metas.iter().map(|m| m.3).sum::<usize>(),
                    "chars": metas.iter().map(|m| m.4).sum::<usize>(),
                    "placeholder_chars": metas.iter().map(|m| m.5).sum::<usize>(),
                    "aggregate_token_savings_estimate": metas.iter().map(|m| m.6).sum::<usize>()
                });
                let mut md = String::new();
                md.push_str("# Memory Stats\n\n```json\n");
                md.push_str(&serde_json::to_string_pretty(&stats)?);
                md.push_str("\n```\n");
                Ok(md)
            }
            MemoryAction::Store { start, end } => {
                if start >= end {
                    return Task::ready(Err(anyhow!(
                        "invalid range: start ({}) must be < end ({})",
                        start,
                        end
                    )));
                }
                let inclusive_end = end - 1;
                let seg_id = thread.update(cx, |thread, thread_cx| {
                    thread.store_memory_segment(start, inclusive_end, thread_cx)
                })?;
                // Read back meta
                let metas = thread.read_with(cx, |t, _| t.memory_segment_metas());
                let meta = metas
                    .into_iter()
                    .find(|m| m.0 == seg_id)
                    .ok_or_else(|| anyhow!("segment {} not found after store", seg_id))?;
                let meta_json = serde_json::json!({
                    "id": meta.0,
                    "start": meta.1,
                    "end": meta.2,
                    "count": meta.3,
                    "chars": meta.4,
                    "placeholder_chars": meta.5,
                    "token_savings_estimate": meta.6,
                    "summary": meta.7,
                    "stored_epoch_ms": meta.8
                });
                let mut md = String::new();
                md.push_str("# Stored Memory Segment\n\n```json\n");
                md.push_str(&serde_json::to_string_pretty(&meta_json)?);
                md.push_str("\n```\n");
                Ok(md)
            }
            MemoryAction::Load {
                id,
                include_messages,
            } => {
                let (meta_json, msgs) =
                    thread.read_with(cx, |t, _| t.load_memory_segment(id))??;
                let mut md = String::new();
                md.push_str("# Memory Segment\n\n```json\n");
                md.push_str(&serde_json::to_string_pretty(&meta_json)?);
                md.push_str("\n```\n");
                if include_messages {
                    md.push_str("\n## Messages\n\n");
                    for (i, m) in msgs.iter().enumerate() {
                        md.push_str(&format!("### Message {}\n\n{}\n\n", i, m));
                    }
                }
                Ok(md)
            }
            MemoryAction::Restore { id } => {
                thread.update(cx, |thread, thread_cx| {
                    thread.restore_memory_segment(id, thread_cx)
                })?;
                let (meta_json, msgs) =
                    thread.read_with(cx, |t, _| t.load_memory_segment(id))??;
                let mut md = String::new();
                md.push_str("# Restored Memory Segment\n\n```json\n");
                md.push_str(&serde_json::to_string_pretty(&meta_json)?);
                md.push_str("\n```\n\n## Messages\n\n");
                for (i, m) in msgs.iter().enumerate() {
                    md.push_str(&format!("### Message {}\n\n{}\n\n", i, m));
                }
                Ok(md)
            }
            MemoryAction::Prune { id } => {
                thread.update(cx, |thread, thread_cx| {
                    thread.prune_memory_segment(id, thread_cx)
                })?;
                let mut md = String::new();
                md.push_str("# Pruned Memory Segment\n\n");
                md.push_str(&format!("Removed segment {}\n", id));
                Ok(md)
            }
        };

        Task::ready(result)
    }
}
