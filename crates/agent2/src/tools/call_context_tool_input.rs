use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Input payload for the indirect `call_context_tool` dynamic tool.
///
/// This indirection allows a single exported MCP tool to invoke one of a small
/// vetted set of internal context–management tools (currently `list_history`
/// and `memory`) without exporting each one separately in environments that
/// only support a limited or static tool manifest.
///
/// Fields:
/// * `name` - The target tool to invoke. Supported values:
///            - "list_history"
///            - "memory"
/// * `arguments` - JSON object containing the concrete tool's input parameters,
///                 matching the schema of `ListHistoryToolInput` or
///                 `MemoryToolInput` respectively. If omitted and `name` is
///                 "list_history", defaults to an empty object (all defaults).
///                 For "memory", arguments are required because an explicit
///                 operation (e.g. store / load / restore) must be specified.
///
/// Unknown tool names or invalid argument shapes are reported as errors by
/// the executor layer that deserializes this struct.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CallContextToolInput {
    /// Target context-compaction tool name ("list_history" | "memory").
    pub name: String,
    /// Raw JSON arguments forwarded to the chosen tool.
    pub arguments: Option<serde_json::Value>,
}
