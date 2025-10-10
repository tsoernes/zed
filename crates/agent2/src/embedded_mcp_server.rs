use anyhow::{Result, anyhow};
use context_server::{
    listener::{McpServer, McpServerTool, ToolResponse},
    types::{ToolAnnotations, ToolResponseContent},
};
use gpui::{App, AppContext, AsyncApp, Entity, Global, ReadGlobal, Task, UpdateGlobal};
use log;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::time::SystemTime;

/// Active thread global accessor. The embedding layer should keep this updated
/// when the user focuses / opens / closes a thread.
pub struct GlobalActiveThread(Option<Entity<Thread>>);

impl Global for GlobalActiveThread {}

impl GlobalActiveThread {
    pub fn set_active_thread(thread: Option<Entity<Thread>>, cx: &mut App) {
        GlobalActiveThread::set_global(cx, GlobalActiveThread(thread));
    }

    pub fn active_thread(cx: &App) -> Option<Entity<Thread>> {
        GlobalActiveThread::global(cx).0.clone()
    }
}

// Re-export of thread so callers can use set_active_thread without importing internal path.
use crate::thread::Thread;

/// Handle to the embedded MCP server for agent2.
pub struct EmbeddedMcpServer {
    _server: Entity<McpServerHandle>,
}

impl Global for EmbeddedMcpServer {}

struct McpServerHandle {
    server: McpServer,
    _write_socket_path_task: Task<()>,
}

/// Initialize the embedded MCP server and register context-management tools.
pub fn init(cx: &mut App) {
    log::info!("agent2::embedded_mcp_server::init called");

    // Initialize GlobalActiveThread immediately to prevent "global not found" errors
    GlobalActiveThread::set_global(cx, GlobalActiveThread(None));
    log::info!("agent2::embedded_mcp_server GlobalActiveThread initialized");

    let task = cx.spawn(async move |cx| {
        log::info!("agent2::embedded_mcp_server spawning MCP server initialization");
        let server = McpServer::new(&cx).await?;
        log::info!("agent2::embedded_mcp_server MCP server created successfully");

        cx.update(|cx| {
            let server_entity = cx.new(|cx| {
                let mut handle = McpServerHandle {
                    server,
                    _write_socket_path_task: Task::ready(()),
                };

                // Register tools
                log::info!("agent2::embedded_mcp_server registering tools");
                handle.server.add_tool(ListHistoryMcpTool);
                log::info!("agent2::embedded_mcp_server registered ListHistoryMcpTool");
                handle.server.add_tool(MemoryMcpTool);
                log::info!("agent2::embedded_mcp_server registered MemoryMcpTool");
                handle.server.add_tool(CallContextMcpTool);
                log::info!("agent2::embedded_mcp_server registered CallContextMcpTool");

                // Persist socket path for external (CLI) access.
                let socket_path = handle.server.socket_path().to_path_buf();
                handle._write_socket_path_task = cx.background_spawn(async move {
                    if let Ok(home) = std::env::var("HOME") {
                        let socket_path_file = std::path::PathBuf::from(home)
                            .join(".zed/embedded_compaction_mcp_socket");
                        if let Some(parent) = socket_path_file.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        match std::fs::write(
                            &socket_path_file,
                            socket_path.to_string_lossy().as_bytes(),
                        ) {
                            Ok(_) => {
                                log::info!(
                                    "agent2 MCP server socket path written to {:?}",
                                    socket_path_file
                                );
                            }
                            Err(e) => {
                                log::error!(
                                    "Failed to write agent2 MCP socket path to {:?}: {}",
                                    socket_path_file,
                                    e
                                );
                            }
                        }
                    }
                });

                handle
            });

            cx.set_global(EmbeddedMcpServer {
                _server: server_entity,
            });
            log::info!("agent2 embedded MCP server initialized successfully");
            anyhow::Ok(())
        })
    });

    log::info!("agent2::embedded_mcp_server init task spawned, will run asynchronously");
    task.detach_and_log_err(cx);
}

// ============================================================================
// ListHistory Tool
// ============================================================================

#[derive(Clone)]
struct ListHistoryMcpTool;

/// Enumerate a slice of the thread's messages with indices, previews, and optional full markdown.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
/// Input schema for the list_history tool. Provides pagination and preview size limits.
struct ListHistoryInput {
    #[serde(default)]
    start: usize,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default = "default_max_chars")]
    max_chars_per_message: usize,
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
struct ListHistoryMessagePreview {
    index: usize,
    role: String,
    chars: usize,
    truncated: bool,
    preview: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ListHistoryOutput {
    total_messages: usize,
    showing_range: String,
    messages_shown: usize,
    messages: Vec<ListHistoryMessagePreview>,
    full_markdown: Option<Vec<String>>,
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
        cx: &mut AsyncApp,
    ) -> Result<ToolResponse<Self::Output>> {
        let thread_entity = match cx.read_global(|g: &GlobalActiveThread, _app| g.0.clone())? {
            Some(t) => t,
            None => {
                let out = ListHistoryOutput {
                    total_messages: 0,
                    showing_range: "0..0".into(),
                    messages_shown: 0,
                    messages: Vec::new(),
                    full_markdown: None,
                };
                let text = "# Conversation History\n\nNo active thread.".to_string();
                return Ok(ToolResponse {
                    content: vec![ToolResponseContent::Text { text }],
                    structured_content: out,
                });
            }
        };

        let result = thread_entity.read_with(cx, |thread, _| {
            let total = thread.messages().len();
            if total == 0 {
                let out = ListHistoryOutput {
                    total_messages: 0,
                    showing_range: "0..0".into(),
                    messages_shown: 0,
                    messages: Vec::new(),
                    full_markdown: None,
                };
                let txt = "# Conversation History\n\nThread is empty.".to_string();
                return (out, txt);
            }

            let start = input.start.min(total.saturating_sub(1));
            let limit = input.limit.clamp(1, 500);
            let end_exclusive = (start + limit).min(total);
            let showing_range = format!("{}..{}", start, end_exclusive.saturating_sub(1));
            let slice = &thread.messages()[start..end_exclusive];

            let mut previews = Vec::with_capacity(slice.len());
            let mut full_markdown = if input.include_full_markdown {
                Some(Vec::with_capacity(slice.len()))
            } else {
                None
            };

            for (offset, msg) in slice.iter().enumerate() {
                let ix = start + offset;
                let md = msg.to_markdown();
                let truncated = md.len() > input.max_chars_per_message;
                let preview = if truncated {
                    let mut s: String = md.chars().take(input.max_chars_per_message).collect();
                    s.push('…');
                    s
                } else {
                    md.clone()
                };
                previews.push(ListHistoryMessagePreview {
                    index: ix,
                    role: msg.role().to_string(),
                    chars: md.len(),
                    truncated,
                    preview,
                });
                if let Some(all) = full_markdown.as_mut() {
                    all.push(md);
                }
            }

            let mut table = String::new();
            writeln!(
                &mut table,
                "| index | role | chars | truncated | preview |\n|-------|------|-------|-----------|---------|"
            )
            .ok();
            for p in &previews {
                let _ = writeln!(
                    &mut table,
                    "| {} | {} | {} | {} | {} |",
                    p.index,
                    p.role,
                    p.chars,
                    p.truncated,
                    escape_pipes(&p.preview)
                );
            }

            let mut txt = String::from("# Conversation History\n\n");
            writeln!(
                &mut txt,
                "Total messages: {}\nShowing range: {}\nMessages shown: {}\n",
                total,
                showing_range,
                previews.len()
            )
            .ok();
            txt.push_str(&table);
            if let Some(all) = &full_markdown {
                txt.push_str("\n\n## Full Markdown\n\n");
                for (i, md) in all.iter().enumerate() {
                    writeln!(&mut txt, "### Message {}\n\n{}", start + i, md).ok();
                }
            }

            let out = ListHistoryOutput {
                total_messages: total,
                showing_range,
                messages_shown: previews.len(),
                messages: previews,
                full_markdown,
            };
            (out, txt)
        });
        let (output, text) = result?;

        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text { text }],
            structured_content: output,
        })
    }
}

fn escape_pipes(s: &str) -> String {
    // Very minimal escaping for markdown table cells containing '|'
    s.replace('|', "\\|")
}

// ============================================================================
// Memory Tool
// ============================================================================

#[derive(Clone)]
struct MemoryMcpTool;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
/// Input schema for the memory tool. Specifies the operation and related parameters.
struct MemoryInput {
    operation: MemoryOperation,
    #[serde(skip_serializing_if = "Option::is_none")]
    start_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    end_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    memory_handle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    #[serde(default)]
    auto: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_preview_chars: Option<usize>,
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
struct MemorySegmentMeta {
    id: u64,
    start: usize,
    end: usize,
    count: usize,
    chars: usize,
    placeholder_chars: usize,
    token_savings_estimate: usize,
    summary: String,
    stored_epoch_ms: u128,
}

#[derive(Debug, Serialize, JsonSchema)]
struct MemoryOutput {
    operation: String,
    success: bool,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    segment: Option<MemorySegmentMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    segments: Option<Vec<MemorySegmentMeta>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    preview: Option<String>,
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
        let thread_entity = match cx.read_global(|g: &GlobalActiveThread, _app| g.0.clone())? {
            Some(t) => t,
            None => {
                let operation = format!("{:?}", input.operation).to_lowercase();
                let out = MemoryOutput {
                    operation,
                    success: false,
                    message: "No active thread context available".into(),
                    segment: None,
                    segments: None,
                    preview: None,
                };
                let text = "# Memory Operation\n\nNo active thread context available.".to_string();
                return Ok(ToolResponse {
                    content: vec![ToolResponseContent::Text { text }],
                    structured_content: out,
                });
            }
        };

        let update_result = thread_entity.update(cx, |thread, cx| -> anyhow::Result<(MemoryOutput, String)> {
            let op_name = format!("{:?}", input.operation).to_lowercase();
            match input.operation {
                MemoryOperation::Store => {
                    let start = input
                        .start_index
                        .ok_or_else(|| anyhow!("start_index required for store"))?;
                    let end = input
                        .end_index
                        .ok_or_else(|| anyhow!("end_index required for store"))?;
                    let id = thread.store_memory_segment(start, end, cx)?;
                    let meta_tuple = thread
                        .memory_segment_metas()
                        .into_iter()
                        .find(|(seg_id, ..)| *seg_id == id)
                        .ok_or_else(|| anyhow!("segment stored but not found"))?;
                    let (
                        seg_id,
                        seg_start,
                        seg_end,
                        seg_count,
                        seg_chars,
                        seg_placeholder_chars,
                        seg_savings,
                        seg_summary,
                        seg_epoch_ms,
                    ) = meta_tuple;
                    let meta = MemorySegmentMeta {
                        id: seg_id,
                        start: seg_start,
                        end: seg_end,
                        count: seg_count,
                        chars: seg_chars,
                        placeholder_chars: seg_placeholder_chars,
                        token_savings_estimate: seg_savings,
                        summary: seg_summary,
                        stored_epoch_ms: seg_epoch_ms,
                    };
                    let out = MemoryOutput {
                        operation: op_name.clone(),
                        success: true,
                        message: format!(
                            "Stored segment id={} range={}..{} count={}",
                            meta.id, meta.start, meta.end, meta.count
                        ),
                        segment: Some(meta),
                        segments: None,
                        preview: None,
                    };
                    let txt = format!(
                        "# Memory Store\n\nStored segment id={} range={}..{} count={} chars={} token_savings_estimate={}\n",
                        out.segment.as_ref().unwrap().id,
                        out.segment.as_ref().unwrap().start,
                        out.segment.as_ref().unwrap().end,
                        out.segment.as_ref().unwrap().count,
                        out.segment.as_ref().unwrap().chars,
                        out.segment.as_ref().unwrap().token_savings_estimate
                    );
                    Ok((out, txt))
                }
                MemoryOperation::Load => {
                    let handle = input
                        .memory_handle
                        .ok_or_else(|| anyhow!("memory_handle required for load"))?;
                    let id: u64 = handle.parse().map_err(|_| anyhow!("memory_handle must be u64"))?;
                    let (meta_json, messages) = thread.load_memory_segment(id)?;
                    let seg_meta = MemorySegmentMeta {
                        id: meta_json["id"].as_u64().unwrap_or(id),
                        start: meta_json["start"].as_u64().unwrap_or(0) as usize,
                        end: meta_json["end"].as_u64().unwrap_or(0) as usize,
                        count: meta_json["count"].as_u64().unwrap_or(0) as usize,
                        chars: meta_json["chars"].as_u64().unwrap_or(0) as usize,
                        placeholder_chars: meta_json["placeholder_chars"].as_u64().unwrap_or(0) as usize,
                        token_savings_estimate: meta_json["token_savings_estimate"].as_u64().unwrap_or(0) as usize,
                        summary: meta_json["summary"].as_str().unwrap_or("").to_string(),
                        stored_epoch_ms: meta_json["stored_epoch_ms"].as_u64().unwrap_or(0) as u128,
                    };
                    let preview = if let Some(max) = input.max_preview_chars {
                        if max > 0 {
                            let joined = messages.join("\n");
                            if joined.len() <= max {
                                Some(joined)
                            } else {
                                let truncated: String = joined.chars().take(max).collect();
                                Some(format!("{truncated}…"))
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    let out = MemoryOutput {
                        operation: op_name.clone(),
                        success: true,
                        message: format!(
                            "Loaded segment id={} chars={} messages={}",
                            seg_meta.id, seg_meta.chars, seg_meta.count
                        ),
                        segment: Some(seg_meta),
                        segments: None,
                        preview,
                    };
                    let mut txt = String::from("# Memory Load\n\n");
                    writeln!(
                        &mut txt,
                        "Loaded segment id={} range={}..{} count={} chars={} token_savings_estimate={}",
                        out.segment.as_ref().unwrap().id,
                        out.segment.as_ref().unwrap().start,
                        out.segment.as_ref().unwrap().end,
                        out.segment.as_ref().unwrap().count,
                        out.segment.as_ref().unwrap().chars,
                        out.segment.as_ref().unwrap().token_savings_estimate
                    )
                    .ok();
                    if let Some(p) = &out.preview {
                        writeln!(&mut txt, "\nPreview:\n{}", p).ok();
                    }
                    Ok((out, txt))
                }
                MemoryOperation::List => {
                    let mut metas = Vec::new();
                    for (seg_id,
                         seg_start,
                         seg_end,
                         seg_count,
                         seg_chars,
                         seg_placeholder_chars,
                         seg_savings,
                         seg_summary,
                         seg_epoch_ms) in thread.memory_segment_metas()
                    {
                        metas.push(MemorySegmentMeta {
                            id: seg_id,
                            start: seg_start,
                            end: seg_end,
                            count: seg_count,
                            chars: seg_chars,
                            placeholder_chars: seg_placeholder_chars,
                            token_savings_estimate: seg_savings,
                            summary: seg_summary,
                            stored_epoch_ms: seg_epoch_ms,
                        });
                    }
                    let out = MemoryOutput {
                        operation: op_name.clone(),
                        success: true,
                        message: format!("Listed {} segments", metas.len()),
                        segment: None,
                        segments: Some(metas),
                        preview: None,
                    };
                    let mut txt = String::from("# Memory List\n\n");
                    if let Some(segs) = &out.segments {
                        if segs.is_empty() {
                            txt.push_str("No archived segments.\n");
                        } else {
                            txt.push_str("| id | range | count | chars | placeholder | savings | stored_epoch_ms | summary |\n");
                            txt.push_str("|----|-------|-------|-------|-------------|---------|-----------------|---------|\n");
                            for s in segs {
                                let _ = writeln!(
                                    &mut txt,
                                    "| {} | {}..{} | {} | {} | {} | {} | {} |",
                                    s.id,
                                    s.start,
                                    s.end,
                                    s.count,
                                    s.chars,
                                    s.placeholder_chars,
                                    s.token_savings_estimate,
                                    escape_pipes(&s.summary)
                                );
                            }
                        }
                    }
                    Ok((out, txt))
                }
                MemoryOperation::Restore => {
                    let handle = input
                        .memory_handle
                        .ok_or_else(|| anyhow!("memory_handle required for restore"))?;
                    let id: u64 = handle.parse().map_err(|_| anyhow!("memory_handle must be u64"))?;
                    thread.restore_memory_segment(id, cx)?;
                    let out = MemoryOutput {
                        operation: op_name.clone(),
                        success: true,
                        message: format!("Restored segment id={}", id),
                        segment: None,
                        segments: None,
                        preview: None,
                    };
                    let txt = format!("# Memory Restore\n\nRestored segment id={}\n", id);
                    Ok((out, txt))
                }
                MemoryOperation::Prune => {
                    let handle = input
                        .memory_handle
                        .ok_or_else(|| anyhow!("memory_handle required for prune"))?;
                    let id: u64 = handle.parse().map_err(|_| anyhow!("memory_handle must be u64"))?;
                    thread.prune_memory_segment(id, cx)?;
                    let out = MemoryOutput {
                        operation: op_name.clone(),
                        success: true,
                        message: format!("Pruned segment id={}", id),
                        segment: None,
                        segments: None,
                        preview: None,
                    };
                    let txt = format!("# Memory Prune\n\nPruned segment id={}\n", id);
                    Ok((out, txt))
                }
            }
        });
        let (output, text) = update_result??;
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text { text }],
            structured_content: output,
        })
    }
}

// ============================================================================
// CallContext Tool (placeholder)
// ============================================================================

#[derive(Clone)]
struct CallContextMcpTool;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
/// Input schema for the call_context_tool. Identifies the tool name and raw JSON input.
struct CallContextInput {
    tool_name: String,
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
        // Placeholder: real implementation would route to a registry of context tools.
        let output = CallContextOutput {
            tool_name: input.tool_name.clone(),
            success: false,
            message: "No context tool registry wired".into(),
        };
        let text = format!(
            "# Call Context Tool\n\nRequested tool: {}\nNot yet wired to execution context.\nInput: {:?}\n",
            input.tool_name, input.input
        );
        Ok(ToolResponse {
            content: vec![ToolResponseContent::Text { text }],
            structured_content: output,
        })
    }
}

// ============================================================================
// Convenience API for external callers
// ============================================================================

/// Set the active thread (Some) or clear it (None).
pub fn set_active_thread(thread: Option<Entity<Thread>>, cx: &mut App) {
    if thread.is_some() {
        log::info!("agent2::embedded_mcp_server::set_active_thread called with Some(thread)");
    } else {
        log::info!("agent2::embedded_mcp_server::set_active_thread called with None");
    }
    GlobalActiveThread::set_active_thread(thread, cx);
}

/// Return the current active thread entity.
pub fn active_thread(cx: &App) -> Option<Entity<Thread>> {
    GlobalActiveThread::active_thread(cx)
}

/// Utility to get current unix epoch ms (used only for testing / future expansion).
#[allow(dead_code)]
fn now_epoch_ms() -> u128 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default()
}
