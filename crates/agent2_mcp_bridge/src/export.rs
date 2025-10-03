/*! TODO(injection): add adapters / helper wrappers for planned message injection tools
    (e.g. add_user_message, add_assistant_message, truncate_after) so they can be
    exported alongside list_history, memory, and rewrite_history once implemented.
Export adapters for exposing internal `AgentTool` implementations (e.g. `list_history`,
`memory`, `rewrite_history`) through an external MCP (Model Context Protocol) server
without coupling the server loop directly to the agent2 internals.

Design goals:
- Keep a thin, generic layer: derive JSON Schema for each tool's input, stash a
  callable closure that will deserialize arguments and execute the tool.
- Avoid hard dependencies on how `ToolCallEventStream` is constructed within the
  server. We delegate creation of that stream to an `EventStreamProvider` trait.
- Provide structured error propagation so server code can map failures to MCP
  error codes.

Typical server usage (pseudo):
    let exported = adapt_tool(arc_tool_clone);
    registry.insert(exported.name.clone(), exported);

    // On tools/list:
    for tool in registry.values() { serialize tool.metadata() }

    // On tools/call:
    let result_json = tool.invoke(raw_args, &mut app, &my_event_stream_provider)?;

The server decides how to associate each invocation with a model/tool use id
and what filesystem or project context to inject into the `ToolCallEventStream`.
*/

use std::sync::Arc;

// Crate name in Cargo.toml is `agent-client-protocol`; Rust normalizes the hyphen to an underscore for the import path.
use agent2::{AgentTool, ToolCallEventStream};
use agent_client_protocol as acp;
use anyhow::{anyhow, Result};
use gpui::App;
use schemars::{schema_for, JsonSchema};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

/// Provides a way for the bridge/server to supply a fresh `ToolCallEventStream`
/// for each invocation. This indirection keeps the export layer agnostic of how
/// tool events are surfaced (logging, UI relays, silent, etc).
pub trait EventStreamProvider: Send + Sync + 'static {
    fn new_stream(&self, tool_name: &str) -> ToolCallEventStream;
}

/// Result envelope returned by `ExportedTool::invoke`.
/// The `output` field is the serialized tool output (the `AgentTool::Output`).
/// The `display` field is an optional end‑user text rendering (usually the same
/// as `output` for primitive types).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvocationResult {
    pub output: Value,
    pub display: Option<String>,
}

/// Metadata describing a tool for listing / discovery.
#[derive(Debug, Clone, Serialize)]
pub struct ToolMetadata {
    pub name: String,
    pub description: String,
    pub kind: acp::ToolKind,
    pub input_schema: Value,
}

/// A dynamically invokable exported tool. Internally holds a closure
/// capturing the concrete `AgentTool` instance.
pub struct ExportedTool {
    metadata: ToolMetadata,
    invoker: Arc<
        dyn Fn(Value, &mut App, &dyn EventStreamProvider) -> Result<InvocationResult> + Send + Sync,
    >,
}

impl ExportedTool {
    pub fn metadata(&self) -> &ToolMetadata {
        &self.metadata
    }

    /// Invoke with raw JSON arguments. Returns a structured result.
    pub fn invoke(
        &self,
        raw_args: Value,
        app: &mut App,
        stream_provider: &dyn EventStreamProvider,
    ) -> Result<InvocationResult> {
        (self.invoker)(raw_args, app, stream_provider)
    }
}

/// Adapt a concrete `AgentTool` implementation into an `ExportedTool`.
///
/// Requirements:
/// - The tool's `Input` type implements `JsonSchema + DeserializeOwned`.
/// - The tool's `Output` type implements `Serialize` (for JSON conversion).
///
/// The closure produced:
/// 1. Deserializes raw JSON into `T::Input`.
/// 2. Allocates a fresh `ToolCallEventStream`.
/// 3. Calls the tool's `run` method synchronously (blocking on the returned Task).
/// 4. Serializes the output into JSON.
pub fn adapt_tool<T>(tool: Arc<T>) -> ExportedTool
where
    T: AgentTool + 'static,
    T::Input: JsonSchema + DeserializeOwned,
    T::Output: Serialize,
{
    // Derive JSON Schema for inputs.
    let schema = schema_for!(T::Input);
    let schema_json = serde_json::to_value(&schema).unwrap_or_else(|_| json!({"type":"object"}));

    // Best effort description: requires an App normally; we produce a placeholder
    // and let the server optionally refine later if it wants a live description().
    let description = format!(
        "{} (exported)",
        std::any::type_name::<T>()
            .rsplit("::")
            .next()
            .unwrap_or(T::name())
    );

    let metadata = ToolMetadata {
        name: T::name().to_string(),
        description,
        kind: T::kind(),
        input_schema: schema_json,
    };

    let name_for_err = metadata.name.clone();

    let invoker = Arc::new(
        move |raw: Value, app: &mut App, stream_provider: &dyn EventStreamProvider| {
            // Deserialize input
            let input: T::Input = serde_json::from_value(raw.clone()).map_err(|e| {
                anyhow!(
                    "Failed to deserialize input for tool '{}': {}. Raw: {}",
                    name_for_err,
                    e,
                    raw
                )
            })?;

            // Create a fresh event stream (server decides the correlation semantics).
            let event_stream = stream_provider.new_stream(T::name());

            // Run tool (blocking for now).
            let task = tool.clone().run(input, event_stream, app);
            let output = pollster::block_on(task)?;

            let output_json = serde_json::to_value(&output).map_err(|e| {
                anyhow!(
                    "Serialization error for tool '{}' output: {}",
                    name_for_err,
                    e
                )
            })?;

            // Provide a simple textual display if it's a primitive string;
            // else caller can pretty-print.
            let display = match &output_json {
                Value::String(s) => Some(s.clone()),
                _ => None,
            };

            Ok(InvocationResult {
                output: output_json,
                display,
            })
        },
    );

    ExportedTool { metadata, invoker }
}

/// Convenience to adapt multiple concrete tools at once.
/// Accepts heterogeneous tools by requiring caller to perform type erasure first
/// if necessary. Usually you will call `adapt_tool` per tool instead.
pub fn adapt_tools<T>(tools: impl IntoIterator<Item = Arc<T>>) -> Vec<ExportedTool>
where
    T: AgentTool + 'static,
    T::Input: JsonSchema + DeserializeOwned,
    T::Output: Serialize,
{
    tools.into_iter().map(|t| adapt_tool(t)).collect()
}

/// Simple event stream provider that generates a unique pseudo tool use id and
/// discards all intermediate updates (suitable for headless / logging-only servers).
///
/// NOTE: This assumes the crate re-exports a constructor for `ToolCallEventStream`.
/// If a different construction path is required in the future, replace this stub.
pub struct NoopEventStreamProvider;

impl EventStreamProvider for NoopEventStreamProvider {
    fn new_stream(&self, tool_name: &str) -> ToolCallEventStream {
        // We generate a random tool call id string; the upstream consumer may ignore it.
        let synthetic_id = acp::ToolCallId(format!("mcp-{}-{}", tool_name, Uuid::new_v4()).into());

        // The real `ToolCallEventStream::new` signature (in agent2) requires:
        //   (LanguageModelToolUseId, ThreadEventStream, Option<Fs>)
        // We do not have direct access to those internal items here. The server integrating
        // this module SHOULD implement its own EventStreamProvider that constructs a proper
        // stream. This fallback intentionally panics to signal misconfiguration if invoked.
        //
        // Rationale: silently swallowing events can mask regressions in tools relying on
        // incremental updates (e.g. progress reporting).
        panic!(
            "NoopEventStreamProvider used but ToolCallEventStream construction is not \
             implemented here. Provide a custom EventStreamProvider in the server layer."
        );

        // Placeholder (unreachable):
        // ToolCallEventStream::noop(synthetic_id)
    }
}

/// Utility to transform a collection of `ExportedTool` into serializable metadata
/// (e.g. for a `tools/list` response).
pub fn list_metadata(tools: &[ExportedTool]) -> Vec<&ToolMetadata> {
    tools.iter().map(|t| &t.metadata).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // A lightweight fake tool for schema/export testing.
    #[derive(Clone)]
    struct FakeTool;

    #[derive(Debug, Serialize, Deserialize, JsonSchema)]
    struct FakeInput {
        msg: String,
        times: usize,
    }

    impl AgentTool for FakeTool {
        type Input = FakeInput;
        type Output = String;

        fn name() -> &'static str {
            "fake_tool"
        }

        fn kind() -> acp::ToolKind {
            acp::ToolKind::Other
        }

        fn description(&self) -> gpui::SharedString {
            "Repeat a message".into()
        }

        fn initial_title(
            &self,
            _input: Result<Self::Input, serde_json::Value>,
            _cx: &mut gpui::App,
        ) -> gpui::SharedString {
            "Fake".into()
        }

        fn run(
            self: Arc<Self>,
            input: Self::Input,
            _event_stream: ToolCallEventStream,
            _cx: &mut gpui::App,
        ) -> gpui::Task<Result<String>> {
            let out = input.msg.repeat(input.times);
            gpui::Task::ready(Ok(out))
        }
    }

    struct DummyProvider;
    impl EventStreamProvider for DummyProvider {
        fn new_stream(&self, _tool_name: &str) -> ToolCallEventStream {
            panic!("Test should not construct real streams in this environment.");
        }
    }

    #[test]
    fn schema_and_metadata_export() {
        let tool = Arc::new(FakeTool);
        let exported = adapt_tool(tool);
        let meta = exported.metadata();
        assert_eq!(meta.name, "fake_tool");
        assert!(meta.input_schema.is_object());
    }

    #[test]
    #[should_panic(expected = "Test should not construct real streams")]
    fn invocation_panics_without_provider_impl() {
        let tool = Arc::new(FakeTool);
        let exported = adapt_tool(tool);
        let mut app = gpui::App::new();
        let _ = exported.invoke(
            json!({"msg":"Hi","times":2}),
            &mut app,
            &DummyProvider, // Panics
        );
    }
}
