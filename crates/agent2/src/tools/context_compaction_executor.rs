use std::sync::Arc;

use anyhow::{Result, anyhow};
use gpui::{AsyncApp, Task, WeakEntity};
use serde_json::Value;

use context_server::agent_tool_adapter::ExternalAgentToolExecutor;
use context_server::listener::ToolResponse;
use context_server::types::ToolResponseContent;

use crate::thread::{AgentTool, Thread};
use crate::tools::{CallContextToolInput, ListHistoryTool, ListHistoryToolInput};

/// Executor that exposes the `list_history` context–compaction
/// tools to the context_server adapter without requiring a direct (and cyclic)
/// dependency from the adapter module back into the agent2 crate.
///
/// It implements the `ExternalAgentToolExecutor` trait so the adapter can
/// invoke these tools when they have been registered dynamically.
///
/// Notes:
/// * This executor constructs fresh tool instances per invocation rather than
///   retrieving the tool out of an active running turn.
/// * Incremental event streaming is not forwarded externally; results are
///   returned as a single text response payload.
/// * If future tools need richer streaming, a small event sink adapter could
///   be added here that buffers intermediate updates.
pub struct ContextCompactionExecutor {
    thread: WeakEntity<Thread>,
}

impl ContextCompactionExecutor {
    pub fn new(thread: WeakEntity<Thread>) -> Self {
        Self { thread }
    }

    fn run_list_history(
        &self,
        args: Option<Value>,
        cx: &mut AsyncApp,
    ) -> Task<Result<ToolResponse<Value>>> {
        let input: ListHistoryToolInput = match args {
            Some(v) if !v.is_null() => match serde_json::from_value(v) {
                Ok(val) => val,
                Err(e) => return Task::ready(Err(anyhow!("invalid list_history input: {e}"))),
            },
            _ => serde_json::from_value(serde_json::json!({})).expect("empty object is valid"),
        };
        let Some(thread) = self.thread.upgrade() else {
            return Task::ready(Err(anyhow!("thread no longer exists")));
        };

        let tool = Arc::new(ListHistoryTool::new(thread.downgrade()));
        // Obtain foreground App by updating the thread entity (provides Context<Thread> -> &mut App)
        let task_result = thread.update(cx, |_, thread_cx| {
            let event_stream = crate::ToolCallEventStream::noop("list_history_exec");
            // Run returns a Task<Result<String>>
            tool.clone().run(input, event_stream, thread_cx)
        });

        let task = match task_result {
            Ok(t) => t,
            Err(e) => return Task::ready(Err(e)),
        };

        cx.spawn(async move |_| match task.await {
            Ok(output) => Ok(ToolResponse {
                content: vec![ToolResponseContent::Text { text: output }],
                structured_content: Value::Null,
            }),
            Err(err) => Err(err),
        })
    }

    fn run_memory(
        &self,
        args: Option<Value>,
        cx: &mut AsyncApp,
    ) -> Task<Result<ToolResponse<Value>>> {
        // Internal-only schema (simplified):
        // {
        //   "action": "store" | "restore" | "prune" | "list" | "stats",
        //   "start": <usize>,   // required for store
        //   "end": <usize>,     // required for store
        //   "id": <u64>         // required for restore | prune
        // }
        let Some(thread) = self.thread.upgrade() else {
            return Task::ready(Err(anyhow!("thread no longer exists")));
        };

        let input = match args {
            Some(v) if v.is_object() => v,
            _ => {
                return Task::ready(Err(anyhow!(
                    "memory tool requires an object arguments payload"
                )));
            }
        };

        let action = input
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("memory tool requires 'action' string field"))?
            .to_string();

        // Spawn foreground work via thread.update for mutating operations and data capture.
        let task_result = thread.update(cx, |thread, thread_cx| {
            let result: Result<String> = match action.as_str() {
                "list" => {
                    let mut segs: Vec<_> = thread.list_memory_segments().iter().collect();
                    segs.sort_by_key(|s| s.id);
                    let json_list: Vec<Value> = segs
                        .iter()
                        .map(|s| {
                            serde_json::json!({
                                "id": s.id,
                                "start": s.start,
                                "end": s.end,
                                "count": s.message_count,
                                "chars": s.message_char_count,
                                "summary": s.summary.as_ref(),
                                "stored_epoch_ms": s.stored_epoch_ms,
                                "placeholder_chars": s.placeholder_char_count,
                                "token_savings_estimate": s.message_char_count.saturating_sub(s.placeholder_char_count)
                            })
                        })
                        .collect();
                    let mut out = String::new();
                    out.push_str("# Stored Memories\n\n```json\n");
                    out.push_str(&serde_json::to_string_pretty(&json_list)?);
                    out.push_str("\n```\n");
                    Ok(out)
                }
                "stats" => {
                    let segs = thread.list_memory_segments();
                    let total_segments = segs.len();
                    let total_messages: usize = segs.iter().map(|s| s.message_count).sum();
                    let total_chars: usize = segs.iter().map(|s| s.message_char_count).sum();
                    let total_placeholder_chars: usize =
                        segs.iter().map(|s| s.placeholder_char_count).sum();
                    let total_savings: usize = segs
                        .iter()
                        .map(|s| s.message_char_count.saturating_sub(s.placeholder_char_count))
                        .sum();
                    let stats = serde_json::json!({
                        "segments": total_segments,
                        "messages": total_messages,
                        "chars": total_chars,
                        "placeholder_chars": total_placeholder_chars,
                        "aggregate_token_savings_estimate": total_savings
                    });
                    let mut out = String::new();
                    out.push_str("# Memory Stats\n\n```json\n");
                    out.push_str(&serde_json::to_string_pretty(&stats)?);
                    out.push_str("\n```\n");
                    Ok(out)
                }
                "store" => {
                    let start = input
                        .get("start")
                        .and_then(|v| v.as_u64())
                        .ok_or_else(|| anyhow!("'store' action requires numeric 'start'"))?
                        as usize;
                    let end = input
                        .get("end")
                        .and_then(|v| v.as_u64())
                        .ok_or_else(|| anyhow!("'store' action requires numeric 'end'"))?
                        as usize;
                    let id = thread.store_memory_segment(start, end, thread_cx)?;
                    let seg = thread
                        .list_memory_segments()
                        .iter()
                        .find(|s| s.id == id)
                        .ok_or_else(|| anyhow!("segment disappeared after store"))?;
                    let meta = serde_json::json!({
                        "id": seg.id,
                        "start": seg.start,
                        "end": seg.end,
                        "count": seg.message_count,
                        "chars": seg.message_char_count,
                        "summary": seg.summary.as_ref(),
                        "stored_epoch_ms": seg.stored_epoch_ms,
                        "placeholder_chars": seg.placeholder_char_count,
                        "token_savings_estimate": seg.message_char_count.saturating_sub(seg.placeholder_char_count)
                    });
                    let mut out = String::new();
                    out.push_str("# Stored Memory Segment\n\n```json\n");
                    out.push_str(&serde_json::to_string_pretty(&meta)?);
                    out.push_str("\n```\n");
                    Ok(out)
                }
                "load" => {
                    let id = input
                        .get("id")
                        .and_then(|v| v.as_u64())
                        .ok_or_else(|| anyhow!("'load' action requires numeric 'id'"))?;
                    let (meta, msgs) = thread.load_memory_segment(id)?;
                    let mut out = String::new();
                    out.push_str("# Loaded Memory Segment\n\n```json\n");
                    out.push_str(&serde_json::to_string_pretty(&meta)?);
                    out.push_str("\n```\n");
                    out.push_str("\n## Messages\n\n");
                    for (i, m) in msgs.iter().enumerate() {
                        out.push_str(&format!("### Message {}\n\n", i));
                        out.push_str(m);
                        out.push_str("\n\n");
                    }
                    Ok(out)
                }
                "restore" => {
                    let id = input
                        .get("id")
                        .and_then(|v| v.as_u64())
                        .ok_or_else(|| anyhow!("'restore' action requires numeric 'id'"))?;
                    thread.restore_memory_segment(id, thread_cx)?;
                    let mut out = String::new();
                    out.push_str("# Restored Memory Segment\n\n");
                    out.push_str(&format!("Restored segment {} into active context.\n", id));
                    Ok(out)
                }
                "prune" => {
                    let id = input
                        .get("id")
                        .and_then(|v| v.as_u64())
                        .ok_or_else(|| anyhow!("'prune' action requires numeric 'id'"))?;
                    thread.prune_memory_segment(id, thread_cx)?;
                    let mut out = String::new();
                    out.push_str("# Pruned Memory Segment\n\n");
                    out.push_str(&format!(
                        "Removed segment {} and its placeholder (if present).\n",
                        id
                    ));
                    Ok(out)
                }
                other => Err(anyhow!(
                    "unsupported memory action '{}'. Allowed: list, stats, store, load, restore, prune",
                    other
                )),
            };
            result
        });

        let task = match task_result {
            Ok(t) => t,
            Err(e) => return Task::ready(Err(e)),
        };

        cx.spawn(async move |_| match task.await {
            Ok(out) => Ok(ToolResponse {
                content: vec![ToolResponseContent::Text { text: out }],
                structured_content: Value::Null,
            }),
            Err(err) => Err(err),
        })
    }

    fn run_call_context_tool(
        &self,
        args: Option<Value>,
        cx: &mut AsyncApp,
    ) -> Task<Result<ToolResponse<Value>>> {
        let input: CallContextToolInput = match args {
            Some(v) if !v.is_null() => match serde_json::from_value(v) {
                Ok(val) => val,
                Err(e) => return Task::ready(Err(anyhow!("invalid call_context_tool input: {e}"))),
            },
            _ => return Task::ready(Err(anyhow!("call_context_tool requires arguments"))),
        };

        match input.name.as_str() {
            "list_history" => self.run_list_history(input.arguments, cx),
            "memory" => self.run_memory(input.arguments, cx),
            other => Task::ready(Err(anyhow!(
                "unsupported target tool '{}' (expected 'list_history' | 'memory')",
                other
            ))),
        }
    }
}

impl ExternalAgentToolExecutor for ContextCompactionExecutor {
    fn execute(
        &self,
        name: &str,
        args: Option<Value>,
        cx: &mut AsyncApp,
    ) -> Task<anyhow::Result<ToolResponse<Value>>> {
        match name {
            "list_history" => self.run_list_history(args, cx),
            "memory" => self.run_memory(args, cx),
            "call_context_tool" => self.run_call_context_tool(args, cx),
            other => Task::ready(Err(anyhow!("unsupported tool: {other}"))),
        }
    }
}

/// Helper to register both compaction tools with an executor. The caller is
/// responsible for producing the JSON Schemas and calling the adapter
/// registration function afterwards.
pub fn context_compaction_executor(thread: WeakEntity<Thread>) -> Arc<ContextCompactionExecutor> {
    Arc::new(ContextCompactionExecutor::new(thread))
}
