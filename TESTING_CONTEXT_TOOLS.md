# Testing Context Management Tools in Zed Assistant

This guide explains how to test the context management tools (`list_history`, `memory`) that are now available in the Zed assistant. The former multiplexer `call_context_tool` has been removed; each tool is invoked directly.

## Prerequisites

1. Build Zed with the latest changes:
   ```bash
   cargo build
   ```

2. Run Zed:
   ```bash
   cargo run
   ```

3. Open the assistant panel (Cmd/Ctrl+?)

## Verifying Tools Are Available

When you start a new assistant conversation, the tools should be registered and available. You can verify this in the logs:

```
INFO  [assistant_tools] Registering context management tools: list_history, memory
INFO  [assistant_tools] Registered ListHistoryTool
INFO  [assistant_tools] Registered MemoryTool
<!-- CallContextTool removal: line removed -->
```

## Testing Each Tool

### 1. ListHistoryTool

This tool lists conversation history with stable indices.

**Test prompt:**
```
Can you list the conversation history? Use the list_history tool.
```

**Expected behavior:**
- The assistant should invoke the `list_history` tool
- You should see output showing message indices, previews, and metadata
- The tool should return a table-like structure with message information

**Parameters to test:**
- `start`: Starting message index (default: 0)
- `limit`: Number of messages (default: 40, max: 500)
- `max_chars_per_message`: Preview length (default: 160, max: 4096)
- `include_full_markdown`: Include full message text (default: false)

### 2. MemoryTool

This tool manages conversation memory (archive, load, list, restore, prune).

**Test prompts:**

**Store (archive) messages:**
```
[Removed: legacy memory tool test. The memory tool has been deleted.]
```

**List archived memories:**
```
[Removed: legacy memory tool test. The memory tool has been deleted.]
```

**Load memory details:**
```
[Removed: legacy memory tool test. The memory tool has been deleted.]
```

**Restore archived messages:**
```
[Removed: legacy memory tool test. The memory tool has been deleted.]
```

**Prune unused memories:**
```
[Removed: legacy memory tool test. The memory tool has been deleted.]
```

**Expected behavior:**
- Store: Creates a memory handle like `mem://session-id/uuid` and replaces messages with a placeholder
- List: Shows all stored memories with metadata
- Load: Returns full serialized content of a memory
- Restore: Re-inserts archived messages back into the conversation
- Prune: Removes orphaned memories

<!-- Section removed: CallContextTool no longer available -->

This is a meta-tool that can invoke either `list_history` or `memory` dynamically.

**Test prompts:**
```
(Deprecated) Previously you could invoke `list_history` via a multiplexer; now invoke `list_history` directly.
```

```
[Removed: legacy memory tool test. The memory tool has been deleted.]
```

**Expected behavior:**
- The tool should dynamically dispatch to the requested context tool
- Output should match what you'd get from calling the tool directly

## Checking Tool Registration

If tools don't appear to be working:

1. **Check logs during startup:**
   ```bash
   cargo run 2>&1 | grep -i "assistant_tools"
   ```

2. **Verify tool names are being called:**
   ```bash
   cargo run 2>&1 | grep -E "(list_history|memory)"
   ```

3. **Check for errors:**
   ```bash
   cargo run 2>&1 | grep -i error
   ```

## Current Status

As of the latest implementation:

- ✅ Tools are registered in the `ToolRegistry` during `assistant_tools::init()`
- ✅ Tools appear in logs when assistant sessions start
- ✅ Tools are enabled by default in the "write" agent profile
- ✅ MCP server also exposes these tools for external access
- ⚠️ Tools return placeholder data when called via MCP (need thread context wiring)
- ❓ Need to verify tools are visible and callable in the assistant UI

## Troubleshooting

### Tools not appearing in assistant

**Possible causes:**
1. Tools need to be explicitly listed in agent profiles
2. Assistant UI might filter tools based on certain criteria
3. Tool registration might be happening after assistant initialization

**Debug steps:**
1. Add more logging to see when tools are queried
2. Check if `Tool::enabled()` returns true for these tools
3. Verify the assistant's tool filtering logic

### Tools returning errors

**Common issues:**
1. Missing required parameters (e.g., `start_index` for memory store)
2. Invalid session IDs or memory handles
3. Index out of bounds

**Solutions:**
- Validate input parameters
- Add better error messages
- Check thread context is properly passed

## Next Steps

1. **Test in live assistant session** - Try the test prompts above
2. **Verify tool invocation** - Check that tools are actually called, not just listed
3. **Check tool output** - Ensure output is formatted correctly in the assistant
4. **Wire up thread context** - Connect tools to actual conversation data
5. (Removed) Memory tool has been deleted; persistence work no longer applies.

## Related Documentation

- [MCP_TOOLS_IMPLEMENTATION.md](./MCP_TOOLS_IMPLEMENTATION.md) - Implementation details
- [TESTING_MCP_TOOLS.md](./TESTING_MCP_TOOLS.md) - Testing via MCP CLI
- [MCP_ARCHITECTURE_CLARIFICATION.md](./MCP_ARCHITECTURE_CLARIFICATION.md) - Architecture overview