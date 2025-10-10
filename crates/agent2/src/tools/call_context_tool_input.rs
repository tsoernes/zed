use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Input payload for the indirect `call_context_tool` dynamic tool.
///
/// This indirection allows a single exported MCP tool to invoke the
/// internal context–management tool `list_history` without exporting it
/// separately in environments that only support a limited or static tool
/// manifest.
///
/// Fields:
/// * `name` - The target tool to invoke. Supported values (currently):
///            - "list_history"
/// * `arguments` - JSON object containing the concrete tool's input parameters,
///                 matching the schema of `ListHistoryToolInput`. If omitted
///                 and `name` is "list_history", defaults to an empty object
///                 (all defaults).
///
/// Unknown tool names or invalid argument shapes are reported as errors by
/// the executor layer that deserializes this struct.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CallContextToolInput {
    /// Target context-compaction tool name ("list_history").
    pub name: String,
    /// Raw JSON arguments forwarded to the chosen tool.
    pub arguments: Option<serde_json::Value>,
}
