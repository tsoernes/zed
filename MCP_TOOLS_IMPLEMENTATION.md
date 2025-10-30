# MCP Tools Implementation - Context Management Tools

## Overview

This document describes the implementation of an embedded MCP (Model Context Protocol) server in Zed that exposes context management tools (`list_history`, `memory`) to external MCP clients.

## Architecture

### Components

1. **Embedded MCP Server** (`crates/agent/src/embedded_mcp_server.rs`)
   - Creates a Unix socket for MCP communication
   - Registers context management tools
   - Handles tool invocation requests from MCP clients
   - Writes socket path to `~/.zed/embedded_compaction_mcp_socket`

2. **Context Management Tools** (`crates/assistant_tools/src/context_management/`)
   - `list_history` - Enumerate conversation history with stable indices
   - `memory` - Store, load, list, restore, and prune conversation segments


3. **CLI Client** (`crates/agent_compaction_cli/`)
   - Command-line interface for invoking MCP tools
   - Connects to the embedded MCP server via Unix socket
   - Supports both structured and text output

### Data Flow

```
┌─────────────────────────────────────────┐
│ Zed Editor                               │
│  ┌──────────────────────────────────┐   │
│  │ Embedded MCP Server              │   │
│  │ - Unix Socket Listener           │   │
│  │ - Tool Registry                  │   │
│  │   • list_history                 │   │
│  │   • memory                       │   │

│  └──────────────────────────────────┘   │
│                                           │
│  ┌──────────────────────────────────┐   │
│  │ Native ToolRegistry              │   │
│  │ - Used internally by Thread      │   │
│  │ - Same tools, different API      │   │
│  └──────────────────────────────────┘   │
└─────────────────────────────────────────┘
              │
              │ JSON-RPC over Unix Socket
              ▼
┌─────────────────────────────────────────┐
│ MCP Clients                             │
│ - agent_compaction_cli                  │
│ - External MCP tools/services           │
│ - Claude Desktop (via MCP)              │
└─────────────────────────────────────────┘
```

## Implementation Details

### Tool Registration

The embedded MCP server is initialized in `agent::init()`:

```rust
pub fn init(fs: Arc<dyn Fs>, cx: &mut gpui::App) {
    thread_store::init(fs, cx);
    embedded_mcp_server::init(cx);
}
```

The server:
1. Creates a temporary Unix socket
2. Registers the three context management tools
3. Writes the socket path to `~/.zed/embedded_compaction_mcp_socket`
4. Starts listening for MCP requests

### Tool Implementations

Each tool implements the `McpServerTool` trait:

```rust
pub trait McpServerTool {
    type Input: DeserializeOwned + JsonSchema;
    type Output: Serialize + JsonSchema;
    const NAME: &'static str;
    
    fn annotations(&self) -> ToolAnnotations;
    fn run(&self, input: Self::Input, cx: &mut AsyncApp) 
        -> impl Future<Output = Result<ToolResponse<Self::Output>>>;
}
```

### Current Status

**✅ Implemented:**
- MCP server infrastructure
- Tool registration and discovery
- Socket communication
- CLI client for testing
- Basic tool schemas and annotations

**⚠️ TODO:**
- Wire tools to actual thread context (currently return placeholders)
- Add thread/conversation context passing
- Implement actual memory storage backend
- Add error handling and validation
- Add authentication/authorization if needed
- Write integration tests

## Usage

### Starting Zed

When Zed starts, the embedded MCP server automatically:
1. Creates a Unix socket
2. Writes the socket path to `~/.zed/embedded_compaction_mcp_socket`
3. Begins listening for MCP requests

### Using the CLI Client

```bash
# List conversation history
agent_compaction_cli list-history --start 0 --limit 10

# Store a memory segment
agent_compaction_cli memory store --start-index 5 --end-index 15 --summary "Important discussion"

# List all memories
agent_compaction_cli memory list

# Load a specific memory
agent_compaction_cli memory load --memory-handle "mem_12345"

# Call with raw JSON
agent_compaction_cli raw-call list_history --arguments '{"start":0,"limit":5}'
```

### MCP Protocol

The server implements the MCP protocol with these methods:

- `tools/list` - Returns list of available tools with schemas
- `tools/call` - Invokes a tool with given parameters

Example `tools/call` request:
```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "tools/call",
  "params": {
    "name": "list_history",
    "arguments": {
      "start": 0,
      "limit": 40,
      "max_chars_per_message": 160,
      "include_full_markdown": false
    }
  }
}
```

Example response:
```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "content": [
      {
        "type": "text",
        "text": "# Conversation History\n\n..."
      }
    ],
    "structured_content": {
      "total_messages": 42,
      "showing_range": "0..40",
      "messages_shown": 40
    }
  }
}
```

## Tool Schemas

### list_history

**Input:**
- `start` (optional, default: 0) - Starting message index
- `limit` (optional, default: 40, max: 500) - Number of messages
- `max_chars_per_message` (optional, default: 160) - Preview character limit
- `include_full_markdown` (optional, default: false) - Include full message content

**Output:**
- Markdown table with message indices, roles, kinds, and previews
- Optional full message content
- Structured data with total counts and ranges

### memory

**Input:**
- `operation` (required) - One of: store, load, list, restore, prune
- `start_index` (required for store) - Starting message index
- `end_index` (required for store) - Ending message index
- `memory_handle` (required for load/restore) - Memory identifier
- `summary` (optional) - Human-readable summary
- `auto` (optional) - Use automatic mode
- `max_preview_chars` (optional) - Preview character limit
- `restore_insert_index` (optional) - Where to insert restored memory
- `remove_placeholder` (optional) - Remove placeholder after restore
- `replace_placeholder_with` (optional) - Replace placeholder with text


- Operation status and result message
- For list: Array of stored memories with metadata
- For store: New memory handle
- For load/restore: Restored content







**Output:**



## Integration with Claude Desktop

To use these tools with Claude Desktop via MCP, add to your MCP settings:

```json
{
  "mcpServers": {
    "zed-context": {
      "command": "socat",
      "args": [
        "UNIX-CONNECT:${HOME}/.zed/embedded_compaction_mcp_socket",
        "STDIO"
      ]
    }
  }
}
```

This allows Claude Desktop to access Zed's context management tools directly.

## Security Considerations

- The Unix socket is currently created in a temporary directory
- Socket path is written to a file in `~/.zed/`
- No authentication is currently implemented
- Access is limited to local processes with file system access
- Consider adding authentication tokens for production use

## Future Enhancements

1. **Thread Context Wiring**
   - Pass active thread/conversation to tools
   - Support multiple concurrent threads
   - Add thread selection mechanisms

2. **Memory Backend**
   - Persistent storage for memories
   - Indexing and search capabilities
   - Memory compression and deduplication

3. **Enhanced CLI**
   - Interactive mode
   - Streaming responses
   - Better error messages and debugging

4. **Security**
   - Authentication tokens
   - Rate limiting
   - Access control lists

5. **Monitoring**
   - Tool usage metrics
   - Performance monitoring
   - Error tracking and logging

## References

- [Model Context Protocol Specification](https://spec.modelcontextprotocol.io/)
- [MCP TypeScript SDK](https://github.com/modelcontextprotocol/typescript-sdk)
- `crates/context_server/` - Zed's MCP implementation
- `crates/agent/src/thread.rs` - Thread and conversation management