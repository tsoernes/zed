use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::{Value, json};

/// Default location where the embedded MCP server socket path is persisted.
const DEFAULT_SOCKET_PATH_FILE: &str = ".zed/embedded_compaction_mcp_socket";

#[derive(Parser, Debug)]
#[command(
    name = "agent_compaction_cli",
    about = "Invoke embedded agent compaction tools (list_history, memory) via the in-process MCP server."
)]
struct Cli {
    /// Override path to file containing the Unix socket path (defaults to .zed/embedded_compaction_mcp_socket)
    #[arg(long)]
    socket_path_file: Option<PathBuf>,

    /// Directly override the Unix socket path (bypasses the socket path file).
    #[arg(long)]
    socket: Option<PathBuf>,

    /// Emit raw JSON-RPC response instead of distilled text (where applicable).
    #[arg(long)]
    raw: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// List a slice of conversation history.
    ListHistory {
        #[arg(long)]
        start: Option<usize>,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        max_chars_per_message: Option<usize>,
        #[arg(long)]
        include_full_markdown: bool,
    },
    /// Perform a memory operation (store, load, list, restore, prune).
    Memory {
        #[arg(value_enum)]
        operation: MemoryOperation,
        #[arg(long)]
        start_index: Option<usize>,
        #[arg(long)]
        end_index: Option<usize>,
        #[arg(long)]
        memory_handle: Option<String>,
        #[arg(long)]
        summary: Option<String>,
        #[arg(long)]
        auto: bool,
        #[arg(long)]
        max_preview_chars: Option<usize>,
        #[arg(long)]
        restore_insert_index: Option<usize>,
        #[arg(long)]
        remove_placeholder: bool,
        #[arg(long)]
        replace_placeholder_with: Option<String>,
    },
    /// Call a tool by raw name + JSON arguments (escape carefully). Supports list_history, memory.
    RawCall {
        /// Tool name: list_history | memory
        name: String,
        /// JSON object string representing arguments.
        #[arg(long)]
        arguments: Option<String>,
    },
}

#[derive(Clone, Debug, ValueEnum)]
enum MemoryOperation {
    Store,
    Load,
    List,
    Restore,
    Prune,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let socket_path = resolve_socket_path(cli.socket.as_ref(), cli.socket_path_file.as_ref())?;
    let (tool_name, arguments) = build_invocation(&cli.command)?;
    let response = call_mcp_tool(&socket_path, &tool_name, arguments)?;
    if cli.raw {
        println!("{}", response.full_json);
        return Ok(());
    }
    println!("{}", response.concatenated_text);
    Ok(())
}

struct ToolInvocationResponse {
    full_json: String,
    concatenated_text: String,
}

fn resolve_socket_path(
    direct: Option<&PathBuf>,
    file_override: Option<&PathBuf>,
) -> Result<PathBuf> {
    if let Some(path) = direct {
        return Ok(path.clone());
    }
    let file = file_override
        .map(|p| p.as_path())
        .unwrap_or_else(|| Path::new(DEFAULT_SOCKET_PATH_FILE));
    let contents = fs::read_to_string(file)
        .with_context(|| format!("reading socket path file {}", file.display()))?;
    let trimmed = contents.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("socket path file {} was empty", file.display()));
    }
    Ok(PathBuf::from(trimmed))
}

fn build_invocation(cmd: &Commands) -> Result<(String, Value)> {
    match cmd {
        Commands::ListHistory {
            start,
            limit,
            max_chars_per_message,
            include_full_markdown,
        } => {
            let mut map = serde_json::Map::new();
            if let Some(v) = start {
                map.insert("start".into(), json!(v));
            }
            if let Some(v) = limit {
                map.insert("limit".into(), json!(v));
            }
            if let Some(v) = max_chars_per_message {
                map.insert("max_chars_per_message".into(), json!(v));
            }
            if *include_full_markdown {
                map.insert("include_full_markdown".into(), json!(true));
            }
            Ok(("list_history".to_string(), Value::Object(map)))
        }
        Commands::Memory {
            operation,
            start_index,
            end_index,
            memory_handle,
            summary,
            auto,
            max_preview_chars,
            restore_insert_index,
            remove_placeholder,
            replace_placeholder_with,
        } => {
            let mut map = serde_json::Map::new();
            map.insert(
                "operation".into(),
                json!(match operation {
                    MemoryOperation::Store => "store",
                    MemoryOperation::Load => "load",
                    MemoryOperation::List => "list",
                    MemoryOperation::Restore => "restore",
                    MemoryOperation::Prune => "prune",
                }),
            );
            if let Some(v) = start_index {
                map.insert("start_index".into(), json!(v));
            }
            if let Some(v) = end_index {
                map.insert("end_index".into(), json!(v));
            }
            if let Some(h) = memory_handle {
                map.insert("memory_handle".into(), json!(h));
            }
            if let Some(s) = summary {
                map.insert("summary".into(), json!(s));
            }
            if *auto {
                map.insert("auto".into(), json!(true));
            }
            if let Some(v) = max_preview_chars {
                map.insert("max_preview_chars".into(), json!(v));
            }
            if let Some(v) = restore_insert_index {
                map.insert("restore_insert_index".into(), json!(v));
            }
            if *remove_placeholder {
                map.insert("remove_placeholder".into(), json!(true));
            }
            if let Some(r) = replace_placeholder_with {
                map.insert("replace_placeholder_with".into(), json!(r));
            }
            validate_memory_input(&map)?;
            Ok(("memory".to_string(), Value::Object(map)))
        }
        Commands::RawCall { name, arguments } => {
            let args_val = if let Some(raw) = arguments {
                if raw.trim().is_empty() {
                    Value::Object(serde_json::Map::new())
                } else {
                    serde_json::from_str(raw)
                        .with_context(|| "parsing --arguments JSON for RawCall")?
                }
            } else {
                Value::Object(serde_json::Map::new())
            };
            if !matches!(name.as_str(), "list_history" | "memory") {
                return Err(anyhow!(
                    "unsupported tool '{}'. Allowed: list_history, memory",
                    name
                ));
            }
            if name == "memory" {
                if !args_val.get("operation").is_some() {
                    return Err(anyhow!(
                        "memory RawCall requires 'operation' field in arguments"
                    ));
                }
            }
            Ok((name.clone(), args_val))
        }
    }
}

fn validate_memory_input(map: &serde_json::Map<String, Value>) -> Result<()> {
    let op = map
        .get("operation")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("memory invocation missing operation"))?;
    match op {
        "store" => {
            if !map.contains_key("start_index") || !map.contains_key("end_index") {
                return Err(anyhow!(
                    "store operation requires --start-index and --end-index"
                ));
            }
        }
        "load" | "restore" => {
            if !map.contains_key("memory_handle") {
                return Err(anyhow!("{} operation requires --memory-handle", op));
            }
        }
        "list" | "prune" => {}
        other => {
            return Err(anyhow!("unknown memory operation: {}", other));
        }
    }
    Ok(())
}

fn call_mcp_tool(
    socket_path: &Path,
    tool_name: &str,
    arguments: Value,
) -> Result<ToolInvocationResponse> {
    let mut stream = UnixStream::connect(socket_path)
        .with_context(|| format!("connecting to {}", socket_path.display()))?;

    // Timeouts are best-effort; ignoring errors if unsupported.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(25)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));

    // Prepare JSON-RPC request
    let request_id = 1;
    let payload = json!({
        "jsonrpc":"2.0",
        "id": request_id,
        "method":"tools/call",
        "params":{
            "name": tool_name,
            "arguments": arguments
        }
    });
    let line = serde_json::to_string(&payload)?;
    stream
        .write_all(line.as_bytes())
        .and_then(|_| stream.write_all(b"\n"))
        .with_context(|| "writing request")?;

    // The server writes newline-delimited JSON responses. Read lines until we find the id we sent.
    let mut reader = BufReader::new(stream);
    let mut buf = String::new();
    let mut raw_response = String::new();
    while reader.read_line(&mut buf)? > 0 {
        let trimmed = buf.trim();
        if trimmed.is_empty() {
            buf.clear();
            continue;
        }
        raw_response.push_str(trimmed);
        // Attempt parse
        if let Ok(val) = serde_json::from_str::<Value>(trimmed) {
            if val.get("id").and_then(|i| i.as_i64()) == Some(request_id) {
                return extract_tool_response(val).map(|concatenated_text| {
                    ToolInvocationResponse {
                        full_json: raw_response,
                        concatenated_text,
                    }
                });
            }
        }
        buf.clear();
    }

    Err(anyhow!(
        "did not receive a valid response for request id {}",
        request_id
    ))
}

fn extract_tool_response(root: Value) -> Result<String> {
    if let Some(err) = root.get("error") {
        return Err(anyhow!("tool error: {}", err));
    }
    let result = root
        .get("result")
        .ok_or_else(|| anyhow!("missing result field"))?;
    let content = result
        .get("content")
        .and_then(|c| c.as_array())
        .ok_or_else(|| anyhow!("missing content array"))?;

    let mut out = String::new();
    for item in content {
        let t = item.get("text").and_then(|t| t.as_str());
        if let Some(text) = t {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(text);
        }
    }
    if out.is_empty() {
        out = "<no text content>".into();
    }
    Ok(out)
}
