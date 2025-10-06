use std::sync::Arc;

use anyhow::{Result, anyhow};
use gpui::{AsyncApp, Task, WeakEntity};
use serde_json::Value;

use context_server::agent_tool_adapter::ExternalAgentToolExecutor;
use context_server::listener::ToolResponse;
use context_server::types::ToolResponseContent;

use crate::thread::{AgentTool, Thread};
use crate::tools::{
    CallContextToolInput, ListHistoryTool, ListHistoryToolInput, MemoryTool, MemoryToolInput,
};

/// Executor that exposes the `list_history` and `memory` context–compaction
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
        let Some(thread) = self.thread.upgrade() else {
            return Task::ready(Err(anyhow!("thread no longer exists")));
        };
        let input: MemoryToolInput = match args {
            Some(v) if !v.is_null() => match serde_json::from_value(v) {
                Ok(val) => val,
                Err(e) => return Task::ready(Err(anyhow!("invalid memory input: {e}"))),
            },
            _ => return Task::ready(Err(anyhow!("memory tool requires arguments"))),
        };

        let tool = Arc::new(MemoryTool::new(thread.downgrade()));
        let task_result = thread.update(cx, |_, thread_cx| {
            let event_stream = crate::ToolCallEventStream::noop("memory_exec");
            tool.clone().run(input, event_stream, thread_cx)
        });

        let task = match task_result {
            Ok(t) => t,
            Err(e) => return Task::ready(Err(e)),
        };

        cx.spawn(async move |_| match task.await {
            Ok(output_string) => Ok(ToolResponse {
                content: vec![ToolResponseContent::Text {
                    text: output_string,
                }],
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
        // Parse the indirection input
        let input: CallContextToolInput = match args {
            Some(v) if !v.is_null() => match serde_json::from_value(v) {
                Ok(val) => val,
                Err(e) => return Task::ready(Err(anyhow!("invalid call_context_tool input: {e}"))),
            },
            _ => return Task::ready(Err(anyhow!("call_context_tool requires arguments"))),
        };

        // Route to the underlying tool using its raw arguments payload.
        match input.name.as_str() {
            "list_history" => {
                // Forward arguments (may be None -> default empty object) to existing handler.
                self.run_list_history(input.arguments, cx)
            }
            "memory" => {
                if input.arguments.is_none() {
                    return Task::ready(Err(anyhow!(
                        "memory tool requires 'arguments' with a valid MemoryToolInput"
                    )));
                }
                self.run_memory(input.arguments, cx)
            }
            other => Task::ready(Err(anyhow!(
                "unsupported target tool '{}' (expected 'list_history' or 'memory')",
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
