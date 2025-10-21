# MCP Architecture Clarification

## Important: Two Separate Tool Systems

This document clarifies a critical architectural distinction that can be confusing.

## The Two Systems

### 1. Native ToolRegistry (Internal to Zed)

**Location:** `crates/assistant_tools/src/`

**Purpose:** Tools used by Zed's Agent/Thread system internally when making LLM requests.

**How it works:**
```
User → Zed Agent → Thread → LanguageModelRequest (with tools array) → Claude API
                                                                         ↓
                                                                    Tool calls
                                                                         ↓
                                              Thread executes tools ← ToolRegistry
```

**Registration:**
```rust
// In assistant_tools::init()
let registry = ToolRegistry::global(cx);
registry.register_tool(ListHistoryTool);
registry.register_tool(MemoryTool);
registry.register_tool(CallContextTool);
```

**Available to:**
- ✅ Zed's internal Agent/Thread system
- ✅ Claude API (via Zed's LLM requests)
- ✅ **YOU (the AI assistant) through Zed's tool system**
- ❌ External MCP clients

**Configuration:**
- Enabled in `assets/settings/default.json` under `agent.profiles.write.tools`
- No additional setup needed
- Already working in your Zed instance

### 2. Embedded MCP Server (External Interface)

**Location:** `crates/agent/src/embedded_mcp_server.rs`

**Purpose:** Expose tools to EXTERNAL clients via Model Context Protocol over Unix socket.

**How it works:**
```
External Client → Unix Socket → MCP Server → Tool Handlers → (Future: Thread context)
     ↑
agent_compaction_cli
Other MCP clients
Claude Desktop (via MCP config)
```

**Registration:**
```rust
// In agent::init()
embedded_mcp_server::init(cx);

// Inside embedded_mcp_server
let mut server = McpServer::new(&cx).await?;
server.add_tool(ListHistoryMcpTool);
server.add_tool(MemoryMcpTool);
server.add_tool(CallContextMcpTool);
```

**Available to:**
- ✅ `agent_compaction_cli` tool
- ✅ External MCP clients (other applications)
- ✅ Claude Desktop (via MCP configuration)
- ❌ NOT for Zed to connect to itself

**Configuration:**
- Socket path: `~/.zed/embedded_compaction_mcp_socket`
- No Zed settings needed (it's internal to Zed)
- CLI clients read socket path from file

## Key Differences

| Aspect | Native ToolRegistry | Embedded MCP Server |
|--------|-------------------|-------------------|
| **Location** | Inside Zed process | Unix socket server in Zed |
| **Protocol** | Direct Rust function calls | JSON-RPC over MCP |
| **Purpose** | Internal tool execution | External tool access |
| **Clients** | Zed's Agent/Thread | CLI, external apps |
| **Thread Access** | Direct | Needs wiring (TODO) |
| **Configuration** | agent.profiles.*.tools | None (automatic) |

## Why Two Systems?

1. **Native ToolRegistry** - Fast, type-safe, direct access for Zed's internal use
2. **Embedded MCP Server** - Standard protocol for external tools/scripts/applications

They expose **the same tools** but through different interfaces.

## Common Confusion: "Why don't I see the tools via MCP?"

**Wrong approach:**
```json
// DON'T add this to Zed settings
"context_servers": {
  "zed-embedded-context": {
    "command": "socat",
    "args": ["-", "UNIX-CONNECT:/tmp/zed-mcp.../mcp.sock"]
  }
}
```

This creates a circular dependency: Zed trying to connect to itself as an external MCP server.

**Right approach:**

The tools are ALREADY available through the native system:
- They're registered in `ToolRegistry::global(cx)`
- They're enabled in the "write" profile
- They're included in LLM requests
- **You (the AI assistant) can already use them**

## For External Access (CLI or other apps)

Use the embedded MCP server via CLI:
```bash
~/code/zed/target/debug/agent_compaction_cli list-history --start 0 --limit 5
```

Or connect via socket:
```bash
SOCKET=$(cat ~/.zed/embedded_compaction_mcp_socket)
echo '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | socat - UNIX-CONNECT:$SOCKET
```

## Current Status

### Native Tools (Internal)
✅ **FULLY WORKING**
- Registered in ToolRegistry
- Enabled in default profiles
- Available to Agent/Thread
- **Should appear in your tool list**

### MCP Server (External)
✅ **INFRASTRUCTURE COMPLETE**
- Socket created and listening
- Tools registered
- CLI client working
- ⚠️ Returns placeholder data (needs thread context wiring)

## Next Steps

### To use tools as the AI assistant:
1. ✅ Already done - tools are registered natively
2. Check if they appear in available tools list
3. If not appearing, check agent profile settings

### To improve external MCP access:
1. Wire MCP tools to actual thread context
2. Add thread selection/context passing
3. Implement real memory storage backend

## Debugging: "Tools not appearing"

If the tools don't appear in your tool list:

1. **Check logs for tool registration:**
   ```
   ListHistoryTool::name() called - returning 'list_history'
   ```

2. **Verify profile has tools enabled:**
   ```bash
   grep -A20 '"write"' ~/.config/zed/settings.json
   # Or check assets/settings/default.json
   ```

3. **Check which profile is active:**
   - Look at Zed's agent panel
   - Default is "write" profile

4. **Restart Zed after code changes**

## Summary

- **For Zed's AI assistant (you):** Tools are available natively through ToolRegistry
- **For external clients:** Use the embedded MCP server via Unix socket
- **Never connect Zed to its own MCP server as an external context server**

The architecture allows both internal (fast, direct) and external (standard protocol) access to the same tools.