use anyhow::Result;
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

// Re-export of thread so callers can use set_active_thread without importing internal path.
use crate::thread::Thread;

/// Active thread global accessor. The embedding layer should keep this updated
/// when the user focuses / opens / closes a thread.
pub struct GlobalActiveThread(Option<Entity<Thread>>);

impl Global for GlobalActiveThread {}

impl GlobalActiveThread {
    pub fn set_active_thread(thread: Option<Entity<Thread>>, cx: &mut App) {
        GlobalActiveThread::set_global(cx, GlobalActiveThread(thread));
    }

    pub fn active_thread(cx: &App) -> Option<Entity<Thread>> {
        <GlobalActiveThread as ReadGlobal>::global(cx).0.clone()
    }
}

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
                // MemoryMcpTool registration deprecated; memory operations now provided by assistant MemoryTool (thread-backed).
                                log::info!("agent2::embedded_mcp_server MemoryMcpTool deprecated; not registered");
                                // Register memory proxy MCP tool to expose thread-backed memory operations externally.
                                handle.server.add_tool(MemoryProxyMcpTool);
                                log::info!("agent2::embedded_mcp_server registered MemoryProxyMcpTool (thread-backed memory)");
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

// Legacy memory MCP tool definitions removed.
// The previous MemoryInput/MemoryOperation/MemorySegmentMeta/MemoryOutput types and their duplicated
// derive attributes caused conflicting trait implementations. They are intentionally replaced
// by the single MemoryProxyOperation / MemoryProxyInput / MemoryProxyOutput trio below.

// Removed legacy impl McpServerTool for MemoryMcpTool (tool no longer registered).

// const NAME removed with legacy MemoryMcpTool.

// Memory MCP tool implementation removed: legacy orphaned methods deleted.
// Memory operations are no longer exposed via agent2 embedded MCP server.
// Reintroduced via MemoryProxyMcpTool which forwards to thread-backed assistant memory APIs.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum MemoryProxyOperation {
    List {
        #[serde(default)]
        limit: Option<usize>,
    },
    Store {
        start: usize,
        end: usize,
        #[serde(default)]
        summary: Option<String>,
    },
    Load {
        id: u64,
        #[serde(default)]
        include_messages: bool,
    },
    Restore {
        id: u64,
    },
    Prune {
        id: u64,
    },
    Stats,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
/// Input schema for the `memory` MCP tool exposing thread-backed memory operations.
/// The `operation` field selects which memory management action to perform
/// (list, store, load, restore, prune, or stats), along with any parameters
/// embedded in its variant.
struct MemoryProxyInput {
    operation: MemoryProxyOperation,
}

#[derive(Debug, Serialize, JsonSchema)]
struct MemoryProxyOutput {
    operation: String,
    success: bool,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    segment: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    segments: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    restored_messages: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stats: Option<serde_json::Value>,
}

#[derive(Clone)]
struct MemoryProxyMcpTool;

impl McpServerTool for MemoryProxyMcpTool {
    type Input = MemoryProxyInput;
    type Output = MemoryProxyOutput;

    const NAME: &'static str = "memory";

    fn annotations(&self) -> ToolAnnotations {
        ToolAnnotations {
            title: Some("Memory Management (Thread-Backed)".to_string()),
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
        let thread_entity = match cx.read_global(|g: &GlobalActiveThread, _| g.0.clone())? {
            Some(t) => t,
            None => {
                let op = format!("{:?}", input.operation).to_lowercase();
                let out = MemoryProxyOutput {
                    operation: op,
                    success: false,
                    message: "No active thread available".into(),
                    segment: None,
                    segments: None,
                    restored_messages: None,
                    stats: None,
                };
                let text = "# Memory\n\nNo active thread available.".to_string();
                return Ok(ToolResponse {
                    content: vec![ToolResponseContent::Text { text }],
                    structured_content: out,
                });
            }
        };

        let op_clone = input.operation.clone();
        // Perform synchronous update inside thread context.
        let result = thread_entity.update(cx, |thread, cx| match op_clone {
            MemoryProxyOperation::List { limit } => {
                let mut metas = thread.memory_segment_metas();
                metas.sort_by_key(|m| m.0);
                if let Some(l) = limit {
                    if metas.len() > l {
                        metas = metas
                            .into_iter()
                            .rev()
                            .take(l)
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .collect();
                    }
                }
                let json_list: Vec<serde_json::Value> = metas
                    .iter()
                    .map(|m| {
                        serde_json::json!({
                            "id": m.0, "start": m.1, "end": m.2, "count": m.3, "chars": m.4,
                            "placeholder_chars": m.5, "token_savings_estimate": m.6,
                            "summary": m.7, "stored_epoch_ms": m.8
                        })
                    })
                    .collect();
                let out = MemoryProxyOutput {
                    operation: "list".into(),
                    success: true,
                    message: format!("{} segments", json_list.len()),
                    segment: None,
                    segments: Some(json_list),
                    restored_messages: None,
                    stats: None,
                };
                let text = "# Memory Segments\n\n```json\n".to_string()
                    + &serde_json::to_string_pretty(out.segments.as_ref().unwrap())?
                    + "\n```\n";
                Ok((out, text))
            }
            MemoryProxyOperation::Stats => {
                let metas = thread.memory_segment_metas();
                let stats_json = serde_json::json!({
                    "segments": metas.len(),
                    "messages": metas.iter().map(|m| m.3).sum::<usize>(),
                    "chars": metas.iter().map(|m| m.4).sum::<usize>()
                });
                let out = MemoryProxyOutput {
                    operation: "stats".into(),
                    success: true,
                    message: "stats retrieved".into(),
                    segment: None,
                    segments: None,
                    restored_messages: None,
                    stats: Some(stats_json.clone()),
                };
                let text = "# Memory Stats\n\n```json\n".to_string()
                    + &serde_json::to_string_pretty(&stats_json)?
                    + "\n```\n";
                Ok((out, text))
            }
            MemoryProxyOperation::Store {
                start,
                end,
                summary: _,
            } => {
                if start >= end {
                    return Err(anyhow::anyhow!("start must be < end"));
                }
                let inclusive_end = end - 1;
                thread.store_memory_segment(start, inclusive_end, cx)?;
                let metas = thread.memory_segment_metas();
                let seg = metas
                    .last()
                    .ok_or_else(|| anyhow::anyhow!("segment not found"))?;
                let seg_json = serde_json::json!({
                    "id": seg.0, "start": seg.1, "end": seg.2, "count": seg.3, "chars": seg.4,
                    "placeholder_chars": seg.5, "token_savings_estimate": seg.6,
                    "summary": seg.7, "stored_epoch_ms": seg.8
                });
                let out = MemoryProxyOutput {
                    operation: "store".into(),
                    success: true,
                    message: "segment stored".into(),
                    segment: Some(seg_json.clone()),
                    segments: None,
                    restored_messages: None,
                    stats: None,
                };
                let text = "# Stored Memory Segment\n\n```json\n".to_string()
                    + &serde_json::to_string_pretty(&seg_json)?
                    + "\n```\n";
                Ok((out, text))
            }
            MemoryProxyOperation::Load {
                id,
                include_messages,
            } => {
                let (meta, messages_md) = thread.load_memory_segment(id)?;
                let out = MemoryProxyOutput {
                    operation: "load".into(),
                    success: true,
                    message: "segment loaded".into(),
                    segment: Some(meta.clone()),
                    segments: None,
                    restored_messages: if include_messages {
                        Some(messages_md)
                    } else {
                        None
                    },
                    stats: None,
                };
                let mut text = "# Memory Segment\n\n```json\n".to_string()
                    + &serde_json::to_string_pretty(&meta)?
                    + "\n```\n";
                if include_messages {
                    if let Some(msgs) = &out.restored_messages {
                        text.push_str("\n## Messages\n\n");
                        for (i, m) in msgs.iter().enumerate() {
                            text.push_str(&format!("### Message {}\n\n{}\n\n", i, m));
                        }
                    }
                }
                Ok((out, text))
            }
            MemoryProxyOperation::Restore { id } => {
                thread.restore_memory_segment(id, cx)?;
                let (meta, messages_md) = thread.load_memory_segment(id)?;
                let out = MemoryProxyOutput {
                    operation: "restore".into(),
                    success: true,
                    message: "segment restored (messages reinserted)".into(),
                    segment: Some(meta.clone()),
                    segments: None,
                    restored_messages: Some(messages_md.clone()),
                    stats: None,
                };
                let mut text = "# Restored Memory Segment\n\n```json\n".to_string()
                    + &serde_json::to_string_pretty(&meta)?
                    + "\n```\n\n## Messages\n\n";
                for (i, m) in messages_md.iter().enumerate() {
                    text.push_str(&format!("### Message {}\n\n{}\n\n", i, m));
                }
                Ok((out, text))
            }
            MemoryProxyOperation::Prune { id } => {
                thread.prune_memory_segment(id, cx)?;
                let out = MemoryProxyOutput {
                    operation: "prune".into(),
                    success: true,
                    message: format!("segment {} pruned", id),
                    segment: None,
                    segments: None,
                    restored_messages: None,
                    stats: None,
                };
                let text = format!("# Pruned Memory Segment\n\nRemoved {}\n", id);
                Ok((out, text))
            }
        });

        let (output, text) = result??;
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
