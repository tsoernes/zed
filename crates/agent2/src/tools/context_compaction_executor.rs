use std::sync::Arc;

use anyhow::{Result, anyhow};
use gpui::{AsyncApp, Task, WeakEntity};
use serde_json::Value;

use context_server::agent_tool_adapter::ExternalAgentToolExecutor;
use context_server::listener::ToolResponse;
use context_server::types::ToolResponseContent;

use crate::thread::Thread;
use crate::tools::CallContextToolInput;

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

// run_list_history removed (ListHistoryTool is now executed via dynamic generic lookup)

// run_memory removed (MemoryAgentTool is executed via dynamic generic lookup)

// run_call_context_tool removed (call_context_tool should become an AgentTool; temporary unsupported)
}

impl ContextCompactionExecutor {
    /// Generic dynamic tool execution:
    /// - Special-case call_context_tool (it delegates to named context tools).
    /// - For any other name, attempt lookup in the thread’s enabled tool map.
    /// - Returns an error if the thread is gone or the tool is not found.
    fn run_generic_tool(
        &self,
        name: &str,
        args: Option<Value>,
        cx: &mut AsyncApp,
    ) -> Task<Result<ToolResponse<Value>>> {
// call_context_tool not yet converted to AgentTool; unsupported until wrapped
        if name == "call_context_tool" {
            return Task::ready(Err(anyhow!("'call_context_tool' not available (pending AgentTool wrapper)")));
        }

        let Some(thread) = self.thread.upgrade() else {
            return Task::ready(Err(anyhow!("thread no longer exists")));
        };

        // Upgrade to foreground to access the thread’s tool set.
        let task_result = thread.update(cx, |thread, thread_cx| {
            // Requires a public accessor on Thread (e.g. thread.tool(name)) added separately.
            #[allow(clippy::let_and_return)]
            let maybe_tool = thread.tool(name);
            if let Some(tool) = maybe_tool {
                // Prepare input JSON (empty object if none supplied).
                let input_json = args.unwrap_or_else(|| Value::Object(serde_json::Map::new()));
                let event_stream =
                    crate::ToolCallEventStream::noop(format!("{}_exec", name));
                Ok(tool.run(input_json, event_stream, thread_cx))
            } else {
                Err(anyhow!("unsupported tool: {name}"))
            }
        });

        let tool_task = match task_result {
            Ok(t) => t,
            Err(e) => return Task::ready(Err(e)),
        };

        cx.spawn(async move |_| match tool_task.await {
            Ok(agent_output) => Ok(ToolResponse {
                content: vec![ToolResponseContent::Text {
                    text: agent_output.llm_output.to_string(),
                }],
                structured_content: agent_output.raw_output,
            }),
            Err(err) => Err(err),
        })
    }
}

impl ExternalAgentToolExecutor for ContextCompactionExecutor {
    fn execute(
        &self,
        name: &str,
        args: Option<Value>,
        cx: &mut AsyncApp,
    ) -> Task<anyhow::Result<ToolResponse<Value>>> {
        // Dynamic generic path (list_history, memory, chat_history, etc. now resolved uniformly).
        self.run_generic_tool(name, args, cx)
    }
}

/// Helper to register both compaction tools with an executor. The caller is
/// responsible for producing the JSON Schemas and calling the adapter
/// registration function afterwards.
pub fn context_compaction_executor(thread: WeakEntity<Thread>) -> Arc<ContextCompactionExecutor> {
    Arc::new(ContextCompactionExecutor::new(thread))
}
