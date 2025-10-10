use anyhow::Result;
use context_server::{
    listener::{McpServer, McpServerTool, ToolResponse},
    types::{ToolAnnotations, ToolResponseContent},
};
use gpui::{App, AppContext, AsyncApp, Context, Entity, Global, Task};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
// Thread state lives in the agent2 crate. We only use its API surface needed for memory ops.
use agent2::thread::Thread;

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
                handle.server.add_tool(MemoryMcpTool);
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
// Memory Tool
// ============================================================================
#[derive(Clone)]
struct MemoryMcpTool;

/// Perform memory operations: store, load, list, restore, or prune conversation segments.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct MemoryInput {
    /// The operation to perform
    operation: MemoryOperation,

    /// Starting message index (for store operation)
    #[serde(skip_serializing_if = "Option::is_none")]
    start_index: Option<usize>,

    /// Ending message index (for store operation)
    #[serde(skip_serializing_if = "Option::is_none")]
    end_index: Option<usize>,

    /// Memory handle identifier (for load/restore operations)
    #[serde(skip_serializing_if = "Option::is_none")]
    memory_handle: Option<String>,

    /// Optional summary text
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,

    /// Whether to use auto mode
    #[serde(default)]
    auto: bool,

    /// Maximum preview characters
    #[serde(skip_serializing_if = "Option::is_none")]
    max_preview_chars: Option<usize>,

    /// Insert index for restore operation
    #[serde(skip_serializing_if = "Option::is_none")]
    restore_insert_index: Option<usize>,

    /// Remove placeholder after restore
    #[serde(default)]
    remove_placeholder: bool,

    /// Replace placeholder with this text
    #[serde(skip_serializing_if = "Option::is_none")]
    replace_placeholder_with: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum MemoryOperation {
    Store,
    Load,
    List,
    Restore,
    Prune,
}

#[derive(Debug, Serialize, JsonSchema)]
struct MemoryOutput {
    operation: String,
    success: bool,
    message: String,
}

impl McpServerTool for MemoryMcpTool {
    type Input = MemoryInput;
    type Output = MemoryOutput;

    const NAME: &'static str = "memory";

    fn annotations(&self) -> ToolAnnotations {
        ToolAnnotations {
            title: Some("Memory Management".to_string()),
            read_only_hint: None,
            destructive_hint: Some(false),
            idempotent_hint: None,
            open_world_hint: None,
        }
    }

    async fn run(
        &self,
        input: Self::Input,
        cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        // Attempt to obtain the currently focused / active thread entity.
        // This assumes some external integration has registered an Entity<Thread>
        // in global state or provided a resolver; if unavailable we return an error.
        // (Real wiring requires the embedding layer to set this before tool execution.)
        let maybe_thread = cx.read(|cx| {
            // Strategy:
            // 1. A global accessor could be introduced later; for now we look for a focused entity id
            //    if such API exists. Placeholder returns None.
            // 2. The caller should inject the thread context before invoking this tool.
            let _ = cx; // suppress unused warning
            None::<Entity<Thread>>
        });

        let op = input.operation;
        let op_name = format!("{:?}", op).to_lowercase();

        if maybe_thread.is_none() {
            let output = MemoryOutput {
                operation: op_name.clone(),
                success: false,
                message: "No active thread context available for memory operation".to_string(),
            };
            let text = format!(
                "# Memory Operation: {}\n\nNo active thread context is available. \
                This tool must be invoked with an active Thread entity.\n",
                op_name
            );
            return Ok(ToolResponse {
                content: vec![ToolResponseContent::Text { text }],
                structured_content: output,
            });
        }

        let thread_entity = maybe_thread.unwrap();

        // Perform operation
        let result: Result<(String, bool, String)> = thread_entity.update(cx, |thread, cx| {
            match op {
                MemoryOperation::Store => {
                    let start = input
                        .start_index
                        .ok_or_else(|| anyhow::anyhow!("start_index required for store"))?;
                    let end = input
                        .end_index
                        .ok_or_else(|| anyhow::anyhow!("end_index required for store"))?;
                    let id = thread.store_memory_segment(start, end, cx)?;
                    Ok((
                        op_name.clone(),
                        true,
                        format!("stored segment id={} range={}..={}", id, start, end),
                    ))
                }
                MemoryOperation::Load => {
                    let handle = input
                        .memory_handle
                        .ok_or_else(|| anyhow::anyhow!("memory_handle required for load"))?;
                    let id: u64 = handle
                        .parse()
                        .map_err(|_| anyhow::anyhow!("memory_handle must be a u64"))?;
                    let (meta, messages) = thread.load_memory_segment(id)?;
                    let mut msg = format!(
                        "loaded segment id={} meta={} messages={}",
                        id,
                        meta,
                        messages.len()
                    );
                    if let Some(max) = input.max_preview_chars {
                        if max > 0 {
                            let joined = messages.join("\n");
                            let preview = if joined.len() <= max {
                                joined
                            } else {
                                format!("{}…", joined.chars().take(max).collect::<String>())
                            };
                            msg.push_str(&format!("\npreview:\n{}", preview));
                        }
                    }
                    Ok((op_name.clone(), true, msg))
                }
                MemoryOperation::List => {
                    let segments = thread.list_memory_segments();
                    let mut lines = Vec::new();
                    for seg in segments {
                        let savings = seg
                            .message_char_count
                            .saturating_sub(seg.placeholder_char_count);
                        lines.push(format!(
                            "id={} range={}..={} count={} chars={} placeholder_chars={} token_savings_estimate={} summary=\"{}\"",
                            seg.id,
                            seg.start,
                            seg.end,
                            seg.message_count,
                            seg.message_char_count,
                            seg.placeholder_char_count,
                            savings,
                            seg.summary
                        ));
                    }
                    Ok((
                        op_name.clone(),
                        true,
                        if lines.is_empty() {
                            "no archived segments".into()
                        } else {
                            lines.join("\n")
                        },
                    ))
                }
                MemoryOperation::Restore => {
                    let handle = input
                        .memory_handle
                        .ok_or_else(|| anyhow::anyhow!("memory_handle required for restore"))?;
                    let id: u64 = handle
                        .parse()
                        .map_err(|_| anyhow::anyhow!("memory_handle must be a u64"))?;
                    thread.restore_memory_segment(id, cx)?;
                    Ok((
                        op_name.clone(),
                        true,
                        format!("restored segment id={}", id),
                    ))
                }
                MemoryOperation::Prune => {
                    let handle = input
                        .memory_handle
                        .ok_or_else(|| anyhow::anyhow!("memory_handle required for prune"))?;
                    let id: u64 = handle
                        .parse()
                        .map_err(|_| anyhow::anyhow!("memory_handle must be a u64"))?;
                    thread.prune_memory_segment(id, cx)?;
                    Ok((op_name.clone(), true, format!("pruned segment id={}", id)))
                }
            }
        });

        let (operation, success, message) = match result {
            Ok(tuple) => tuple,
            Err(e) => (op_name.clone(), false, e.to_string()),
        };

        // Human-readable text response
        let text = if success {
            format!(
                "# Memory Operation: {}\n\nSuccess: {}\n{}\n",
                operation, success, message
            )
        } else {
            format!(
                "# Memory Operation: {}\n\nSuccess: {}\nError: {}\n",
                operation, success, message
            )
        };

        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text { text }],
            structured_content: MemoryOutput {
                operation,
                success,
                message,
            },
        })
    }
}
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
