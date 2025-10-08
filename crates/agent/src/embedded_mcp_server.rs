use anyhow::Result;
use context_server::{
    listener::{McpServer, McpServerTool, ToolResponse},
    types::{ToolAnnotations, ToolResponseContent},
};
use gpui::{App, AppContext, AsyncApp, Entity, Global, Task};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Global handle to the embedded MCP server that exposes context management tools
pub struct EmbeddedMcpServer {
    _server: Entity<McpServerHandle>,
}

impl Global for EmbeddedMcpServer {}

struct McpServerHandle {
    server: McpServer,
    _write_socket_path_task: Task<()>,
}

pub fn init(cx: &mut App) {
    let task = cx.spawn(async move |cx| {
        let server = McpServer::new(&cx).await?;

        cx.update(|cx| {
            let server_entity = cx.new(|cx| {
                let mut handle = McpServerHandle {
                    server,
                    _write_socket_path_task: Task::ready(()),
                };

                // Register tools
                handle.server.add_tool(ListHistoryMcpTool);

                handle.server.add_tool(CallContextMcpTool);

                // Write socket path to file for CLI access
                let socket_path = handle.server.socket_path().to_path_buf();
                handle._write_socket_path_task = cx.background_spawn(async move {
                    if let Ok(home) = std::env::var("HOME") {
                        let socket_path_file = std::path::PathBuf::from(home)
                            .join(".zed/embedded_compaction_mcp_socket");
                        if let Some(parent) = socket_path_file.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        if let Err(e) = std::fs::write(
                            &socket_path_file,
                            socket_path.to_string_lossy().as_bytes(),
                        ) {
                            log::error!(
                                "Failed to write MCP socket path to {:?}: {}",
                                socket_path_file,
                                e
                            );
                        } else {
                            log::info!("MCP server socket path written to {:?}", socket_path_file);
                        }
                    }
                });

                handle
            });

            cx.set_global(EmbeddedMcpServer {
                _server: server_entity,
            });

            log::info!("Embedded MCP server initialized");
            anyhow::Ok(())
        })
    });

    task.detach_and_log_err(cx);
}

// ============================================================================
// ListHistory Tool
// ============================================================================

#[derive(Clone)]
struct ListHistoryMcpTool;

/// Enumerate a slice of the thread's messages with stable indices, lightweight previews, and optional full markdown.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct ListHistoryInput {
    /// Inclusive starting message index (default 0)
    #[serde(default)]
    start: usize,

    /// Number of messages to enumerate (default 40, clamped 1..500)
    #[serde(default = "default_limit")]
    limit: usize,

    /// Preview character cap per message (default 160, clamped 16..4096)
    #[serde(default = "default_max_chars")]
    max_chars_per_message: usize,

    /// If true, appends full text of each listed message after the table
    #[serde(default)]
    include_full_markdown: bool,
}

fn default_limit() -> usize {
    40
}

fn default_max_chars() -> usize {
    160
}

#[derive(Debug, Serialize, JsonSchema)]
struct ListHistoryOutput {
    // For structured consumption
    total_messages: usize,
    showing_range: String,
    messages_shown: usize,
}

impl McpServerTool for ListHistoryMcpTool {
    type Input = ListHistoryInput;
    type Output = ListHistoryOutput;

    const NAME: &'static str = "list_history";

    fn annotations(&self) -> ToolAnnotations {
        ToolAnnotations {
            title: Some("List Conversation History".to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: None,
        }
    }

    async fn run(
        &self,
        input: Self::Input,
        _cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        // For now, return a placeholder response
        // This will need to be wired up to the actual thread/conversation history
        let output = ListHistoryOutput {
            total_messages: 0,
            showing_range: format!("{}..{}", input.start, input.start),
            messages_shown: 0,
        };

        let text = format!(
            "# Conversation History\n\n\
            Note: This tool requires access to an active thread context.\n\
            Total messages: {}\n\
            Requested range: start={}, limit={}\n",
            output.total_messages, input.start, input.limit
        );

        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text { text }],
            structured_content: output,
        })
    }
}

// ============================================================================
// Memory Tool (removed)
// ============================================================================
// The former in-repo MCP 'memory' tool (store/load/list/restore/prune conversation
// segments) has been fully removed. This placeholder block remains only to
// preserve line number stability for downstream patches and to document the
// deprecation decision.
// ============================================================================
// CallContextTool
// ============================================================================

#[derive(Clone)]
struct CallContextMcpTool;

/// Call a registered context tool with the given name and input.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct CallContextInput {
    /// The name of the context tool to call
    tool_name: String,

    /// Input parameters for the tool (structure depends on the tool)
    #[serde(default)]
    input: serde_json::Value,
}

#[derive(Debug, Serialize, JsonSchema)]
struct CallContextOutput {
    tool_name: String,
    success: bool,
    message: String,
}

impl McpServerTool for CallContextMcpTool {
    type Input = CallContextInput;
    type Output = CallContextOutput;

    const NAME: &'static str = "call_context_tool";

    fn annotations(&self) -> ToolAnnotations {
        ToolAnnotations {
            title: Some("Call Context Tool".to_string()),
            read_only_hint: None,
            destructive_hint: None,
            idempotent_hint: None,
            open_world_hint: None,
        }
    }

    async fn run(
        &self,
        input: Self::Input,
        _cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        // For now, return a placeholder response
        // This will need to be wired up to the actual context tool registry
        let output = CallContextOutput {
            tool_name: input.tool_name.clone(),
            success: false,
            message: format!(
                "Context tool '{}' requires active thread context",
                input.tool_name
            ),
        };

        let text = format!(
            "# Call Context Tool: {}\n\n\
            Note: This tool requires access to an active thread context.\n\
            Tool: {}\n\
            Input: {:?}\n",
            input.tool_name, input.tool_name, input.input
        );

        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text { text }],
            structured_content: output,
        })
    }
}
