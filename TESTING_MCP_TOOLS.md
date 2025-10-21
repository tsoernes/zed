# Testing MCP Tools - Quick Start Guide

This guide shows how to test the embedded MCP server that exposes Zed's context management tools.

## Prerequisites

1. Build Zed with the embedded MCP server:
   ```bash
   cd ~/code/zed
   cargo build --package zed
   cargo build --package agent_compaction_cli
   ```

2. Start Zed:
   ```bash
   ./target/debug/zed
   ```

3. Verify the MCP server is running:
   ```bash
   ls -lah ~/.zed/embedded_compaction_mcp_socket
   cat ~/.zed/embedded_compaction_mcp_socket
   ```

   You should see output like:
   ```
   /tmp/zed-mcpXXXXXX/mcp.sock
   ```

## Testing with CLI

The `agent_compaction_cli` tool connects to the embedded MCP server via Unix socket.

**Important:** Run the CLI from your home directory (`~`) so it finds the socket path file at `.zed/embedded_compaction_mcp_socket`:

```bash
cd ~
~/code/zed/target/debug/agent_compaction_cli list-history --start 0 --limit 5
```

### Test Commands

#### List conversation history
```bash
# Basic usage
~/code/zed/target/debug/agent_compaction_cli list-history --start 0 --limit 5

# With full markdown output
~/code/zed/target/debug/agent_compaction_cli list-history \
  --start 0 \
  --limit 10 \
  --max-chars-per-message 200 \
  --include-full-markdown

# Get raw JSON response
~/code/zed/target/debug/agent_compaction_cli --raw \
  list-history --start 0 --limit 5
```

#### Memory operations
```bash
# List stored memories
~/code/zed/target/debug/agent_compaction_cli memory list

# Store a memory (requires active thread)
~/code/zed/target/debug/agent_compaction_cli memory store \
  --start-index 5 \
  --end-index 15 \
  --summary "Important discussion about X"

# Load a memory
~/code/zed/target/debug/agent_compaction_cli memory load \
  --memory-handle "mem_12345"

# Prune old memories
~/code/zed/target/debug/agent_compaction_cli memory prune
```

#### Raw JSON-RPC calls
```bash
# Call with custom JSON arguments
~/code/zed/target/debug/agent_compaction_cli --raw \
  raw-call list_history \
  --arguments '{"start":0,"limit":3,"include_full_markdown":true}'

~/code/zed/target/debug/agent_compaction_cli --raw \
  raw-call memory \
  --arguments '{"operation":"list"}'
```

#### Specify socket directly
```bash
# If you need to specify the socket path manually:
~/code/zed/target/debug/agent_compaction_cli \
  --socket /tmp/zed-mcpXXXXXX/mcp.sock \
  list-history --start 0 --limit 5
```

## Current Status

✅ **Working:**
- MCP server starts automatically with Zed
- Unix socket communication
- Tool discovery (`tools/list`)
- Tool invocation (`tools/call`)
- CLI client can connect and call tools

⚠️ **Returns Placeholder Data:**
The tools currently return placeholder responses because they need to be wired to actual thread context:

```
# Conversation History

Note: This tool requires access to an active thread context.
Total messages: 0
Requested range: start=0, limit=5
```

## Testing MCP Protocol Directly

You can test the MCP protocol directly using `socat` or `nc`:

```bash
# Get the socket path
SOCKET=$(cat ~/.zed/embedded_compaction_mcp_socket)

# List available tools
echo '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}' | \
  socat - UNIX-CONNECT:$SOCKET

# Call list_history
echo '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"list_history","arguments":{"start":0,"limit":5}}}' | \
  socat - UNIX-CONNECT:$SOCKET
```

## Integration with Zed Settings

The embedded MCP server has been added to your Zed settings:

```json
{
  "context_servers": {
    "zed-embedded-context": {
      "enabled": true,
      "source": "custom",
      "command": "socat",
      "args": ["-", "UNIX-CONNECT:/tmp/zed-mcpXXXXXX/mcp.sock"],
      "env": {}
    }
  }
}
```

**Note:** You'll need to restart Zed for it to load this context server and make the tools available to the AI assistant.

## Troubleshooting

### Socket not found
```
Error: connecting to /tmp/zed-mcpXXXXXX/mcp.sock
Caused by: No such file or directory (os error 2)
```

**Solutions:**
1. Make sure Zed is running
2. Check the socket path: `cat ~/.zed/embedded_compaction_mcp_socket`
3. Verify socket exists: `ls -lah /tmp/zed-mcp*/mcp.sock`
4. Run CLI from home directory: `cd ~ && ~/code/zed/target/debug/agent_compaction_cli ...`

### No response from server
- Check Zed logs for errors
- Verify `socat` is installed: `which socat`
- Try using `nc` instead: `nc -U $SOCKET`

### Tools not available in Zed UI
- Restart Zed to load the new context server configuration
- Check that `zed-embedded-context` is enabled in settings
- Look for context server connection errors in Zed logs

## Expected Response Format

### Successful Response
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
    "isError": false,
    "structuredContent": {
      "total_messages": 0,
      "showing_range": "0..0",
      "messages_shown": 0
    }
  }
}
```

### Error Response
```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "error": {
    "code": -32601,
    "message": "Tool not found: unknown_tool"
  }
}
```

## Next Steps

To make these tools fully functional, they need to be wired to:
1. Active thread/conversation context
2. Persistent memory storage backend
3. Tool registry for `call_context_tool`

See `MCP_TOOLS_IMPLEMENTATION.md` for implementation details.