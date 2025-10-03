/*!
Main entry point for the `agent2_mcp_bridge` binary.

This is a SKELETON implementation of a JSON-RPC / MCP-style server whose goal is
to expose agent2 context-management tools (`list_history`, `memory`, `rewrite_history`)
to external clients. It wires together:

1. JSON-RPC message loop over stdin/stdout (line-delimited JSON for simplicity).
2. Basic method handling:
   - initialize
   - tools/list
   - tools/call
3. A registry of dynamically adapted tools (`ExportedTool`) produced by the
   adapter layer in `export.rs`.

Current Status / TODOs:
- Thread bootstrap is not implemented (requires constructing a `Project`, `ProjectContext`,
  `ContextServerRegistry`, and then a `Thread`, mirroring editor runtime).
- Real `EventStreamProvider` not yet implemented (the adapter layer will panic
  if it tries to build a `ToolCallEventStream` using the placeholder).
- The three target tools are not yet populated because we cannot (in this
  isolated skeleton) assemble the full environment they depend on.
- Streaming / incremental tool call updates are not yet surfaced. The server
  responds only with final results for `tools/call`.

You should:
1. Replace `bootstrap_bridge_state` with logic that creates or reuses a `Thread`
   and fetches tool instances for `list_history`, `memory`, `rewrite_history`.
2. Implement a concrete `EventStreamProvider` that creates a valid
   `ToolCallEventStream` (see agent2 internals where these are normally built).
3. Potentially add auxiliary injection tools (e.g., add_user_message, add_assistant_message)
   so external callers can seed the conversation before compression operations.

Design Notes:
- The server reads all of stdin into memory (simple). You may refactor to an
  incremental reader (BufRead lines) for streaming input.
- Each incoming JSON-RPC request is assumed single-line JSON (a pragmatic
  simplification—frame-based parsing can be added later).
- Errors are mapped to JSON-RPC error objects with code 400 (client) or 500 (server).
*/

use std::collections::HashMap;
use std::io::{self, Read};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use env_logger::Env;
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use gpui::App;

mod export;
use export::{adapt_tool, EventStreamProvider, ExportedTool, InvocationResult};

// -------------------------------------------------------------------------------------
// JSON-RPC types
// -------------------------------------------------------------------------------------

#[derive(Serialize)]
struct RpcSuccess {
    jsonrpc: &'static str,
    id: Value,
    result: Value,
}

#[derive(Serialize)]
struct RpcError {
    jsonrpc: &'static str,
    id: Value,
    error: RpcErrorBody,
}

#[derive(Serialize)]
struct RpcErrorBody {
    code: i32,
    message: String,
}

// -------------------------------------------------------------------------------------
// Bridge state & tool registry
// -------------------------------------------------------------------------------------

struct BridgeState {
    /// Name → exported tool
    tools: HashMap<String, ExportedTool>,
    /// Whether initialization of internal agent thread succeeded
    ready: bool,
}

/// Placeholder event stream provider.
///
/// IMPORTANT: Replace this with an implementation that constructs a real
/// `ToolCallEventStream` if you want intermediate progress / multi-part
/// tool updates. For now, we never actually call into the adapter's
/// invocation path that needs it (since we cannot bootstrap the real agent
/// thread here). Once thread bootstrap is in place, adapt this provider.
struct PlaceholderEventStreamProvider;

impl EventStreamProvider for PlaceholderEventStreamProvider {
    fn new_stream(&self, tool_name: &str) -> agent2::ToolCallEventStream {
        panic!(
            "EventStreamProvider not implemented for tool '{}'. Replace PlaceholderEventStreamProvider \
             with a concrete implementation that builds ToolCallEventStream.",
            tool_name
        );
    }
}

// -------------------------------------------------------------------------------------
// Skeleton bootstrap logic
// -------------------------------------------------------------------------------------

fn bootstrap_bridge_state(app: &mut App) -> Result<BridgeState> {
    // NOTE:
    // A full bootstrap (Project + ProjectContext + ContextServerRegistry + Templates + Thread)
    // requires mirroring internal editor initialization that pulls in many subsystems.
    // For this bridge we perform a best‑effort lightweight bootstrap. If any mandatory
    // component cannot be initialized, we fall back to an empty tool registry (ready = false).
    //
    // Steps (simplified / fallible):
    // 1. Attempt to obtain a minimal in‑memory project (or stub) via helper in agent2 (if exposed).
    // 2. Create a Thread entity and register default tools.
    // 3. Locate the three context management tools (list_history, memory, rewrite_history).
    // 4. Adapt them via `adapt_tool` adding to the tool map.
    //
    // Because the full internal constructors are not directly available in this isolated context,
    // this scaffold leaves TODOs where deep integration is required. The structure, however,
    // reflects the final state expected by the rest of the server.

    let mut tools = HashMap::new();
    let mut ready = false;

    // --- BEGIN bootstrap sketch (non-functional placeholder) ---
    //
    // Pseudocode for when internal constructors are accessible:
    //
    // let (thread_entity, fs_arc) = app.update(|cx| {
    //     // create or load project + dependencies ...
    //     let thread = Thread::new(project, project_context, context_server_registry, templates, None, cx);
    //     thread.add_default_tools(environment_adapter, cx);
    //     cx.new(|_| thread)
    // })?;
    //
    // let exported = {
    //     // Acquire concrete tool objects out of thread's registry if accessible,
    //     // downcast to their concrete types, wrap each Arc<T> using adapt_tool.
    // };
    // for t in exported {
    //     tools.insert(t.metadata().name.clone(), t);
    // }
    //
    // ready = true;
    //
    // --- END bootstrap sketch ---

    // Until the above is fully implemented we expose only a stub informational tool so
    // clients can detect bridge readiness programmatically.
    struct StubStatusTool;
    #[derive(serde::Deserialize, schemars::JsonSchema)]
    struct StubStatusInput {}
    #[derive(serde::Serialize)]
    struct StubStatusOutput {
        ready: bool,
        available: Vec<String>,
    }
    impl agent2::AgentTool for StubStatusTool {
        type Input = StubStatusInput;
        type Output = StubStatusOutput;
        fn name() -> &'static str {
            "bridge_status"
        }
        fn kind() -> agent_client_protocol::ToolKind {
            agent_client_protocol::ToolKind::Other
        }
        fn description(&self) -> gpui::SharedString {
            "Report MCP bridge readiness and which context tools are registered.".into()
        }
        fn initial_title(
            &self,
            _input: Result<Self::Input, serde_json::Value>,
            _cx: &mut App,
        ) -> gpui::SharedString {
            "Bridge status".into()
        }
        fn run(
            self: Arc<Self>,
            _input: Self::Input,
            _event_stream: agent2::ToolCallEventStream,
            _cx: &mut App,
        ) -> gpui::Task<Result<Self::Output>> {
            // The output will be filled after we know tool names (in closure below)
            let output = StubStatusOutput {
                ready: false,
                available: vec![],
            };
            gpui::Task::ready(Ok(output))
        }
    }
    let status_tool = adapt_tool(Arc::new(StubStatusTool));
    tools.insert(status_tool.metadata().name.clone(), status_tool);

    Ok(BridgeState { tools, ready })
}

// -------------------------------------------------------------------------------------
// Request handling
// -------------------------------------------------------------------------------------

/// Handle the "initialize" method.
fn handle_initialize(id: Value) -> RpcSuccess {
    RpcSuccess {
        jsonrpc: "2.0",
        id,
        result: json!({
          "protocolVersion": "2024-11-05",
          "serverInfo": { "name": "agent2-context-tools", "version": "0.1.0" },
          "capabilities": {
            "tools": { "list": true, "call": true },
            "logging": { "levels": ["debug"], "alwaysOn": true }
          }
        }),
    }
}

/// Handle "tools/list".
fn handle_tools_list(id: Value, state: &BridgeState) -> RpcSuccess {
    let tools: Vec<Value> = state
        .tools
        .values()
        .map(|t| {
            let meta = t.metadata();
            json!({
              "name": meta.name,
              "description": meta.description,
              "schemaVersion": 1,
              "inputSchema": meta.input_schema,
            })
        })
        .collect();

    RpcSuccess {
        jsonrpc: "2.0",
        id,
        result: json!({ "tools": tools }),
    }
}

/// Handle "tools/call".
///
/// Returns an RpcSuccess (with `result`) on success, or a RpcError on failure.
fn handle_tools_call(
    id: Value,
    params: &Value,
    state: &BridgeState,
    app: &mut App,
    stream_provider: &dyn EventStreamProvider,
) -> Result<RpcSuccess> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Missing string field 'name'"))?;

    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let tool = state
        .tools
        .get(name)
        .ok_or_else(|| anyhow!("Unknown tool '{}'", name))?;

    if !state.ready {
        return Err(anyhow!(
            "Bridge not yet fully initialized; tool '{}' unavailable",
            name
        ));
    }

    let InvocationResult { output, display } = tool.invoke(args, app, stream_provider)?;

    let content_block = if let Some(text) = display {
        // Provide both structured output and a top-level text block
        json!([
            { "type": "text", "text": text },
            { "type": "json", "json": output }
        ])
    } else {
        json!([
            { "type": "json", "json": output }
        ])
    };

    Ok(RpcSuccess {
        jsonrpc: "2.0",
        id,
        result: json!({ "content": content_block }),
    })
}

// -------------------------------------------------------------------------------------
// Main loop
// -------------------------------------------------------------------------------------

fn main() -> Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();
    info!("Starting agent2 context tools MCP bridge (skeleton)");

    let mut app = App::new();
    let bridge_state = Arc::new(Mutex::new(bootstrap_bridge_state(&mut app)?));
    let stream_provider: Arc<dyn EventStreamProvider> = Arc::new(PlaceholderEventStreamProvider);

    // Read entire stdin (simple; for production consider a streaming loop).
    let mut buf = String::new();
    io::stdin().read_to_string(&mut buf)?;
    for (line_no, line) in buf.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let parsed: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                warn!("Ignoring line {}: JSON parse error: {}", line_no + 1, e);
                continue;
            }
        };

        let id = parsed.get("id").cloned().unwrap_or(Value::Null);
        let method = parsed
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("<missing>");

        let response_value = {
            let mut state_guard = bridge_state.lock().unwrap();
            match method {
                "initialize" => Either::Ok(handle_initialize(id)),
                "tools/list" => Either::Ok(handle_tools_list(id, &state_guard)),
                "tools/call" => {
                    let params = parsed.get("params").cloned().unwrap_or(json!({}));
                    match handle_tools_call(
                        id,
                        &params,
                        &state_guard,
                        &mut app,
                        stream_provider.as_ref(),
                    ) {
                        Ok(success) => Either::Ok(success),
                        Err(err) => Either::Err(RpcError {
                            jsonrpc: "2.0",
                            id,
                            error: RpcErrorBody {
                                code: 500,
                                message: err.to_string(),
                            },
                        }),
                    }
                }
                _ => Either::Err(RpcError {
                    jsonrpc: "2.0",
                    id,
                    error: RpcErrorBody {
                        code: 400,
                        message: format!("Unsupported method: {method}"),
                    },
                }),
            }
        };

        match response_value {
            Either::Ok(success) => {
                if let Err(e) = serde_json::to_writer(io::stdout(), &success) {
                    error!("Failed to write success response: {e}");
                } else {
                    print!("\n");
                }
            }
            Either::Err(err_resp) => {
                if let Err(e) = serde_json::to_writer(io::stdout(), &err_resp) {
                    error!("Failed to write error response: {e}");
                } else {
                    print!("\n");
                }
            }
        }
    }

    info!("Shutting down MCP bridge");
    Ok(())
}

// -------------------------------------------------------------------------------------
// Utility enum (local Either to avoid pulling a crate for one use case).
// -------------------------------------------------------------------------------------

enum Either<L, R> {
    Ok(L),
    Err(R),
}

// -------------------------------------------------------------------------------------
// (Optional) Future extension hooks (placeholders)
// -------------------------------------------------------------------------------------

/// Register (after proper bootstrap) the context-management tools by name.
/// Kept as a separated function stub for clarity.
///
/// Expected steps once real bootstrap is implemented:
///  1. Acquire thread reference & extract tool instances (or create them).
///  2. Wrap each with `adapt_tool(Arc::new(tool_instance))`.
///  3. Insert into `state.tools`.
#[allow(dead_code)]
fn register_context_tools(_state: &mut BridgeState, _app: &mut App) -> Result<()> {
    // TODO: Implement after real Thread bootstrap.
    Ok(())
}
