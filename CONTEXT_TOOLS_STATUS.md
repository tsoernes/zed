# Context Management Tools - Implementation Status

**Date:** 2025-10-06  
**Branch:** `memory-tools`  
**Status:** ✅ Tools Registered and Enabled

## Overview

Two context management tools have been successfully implemented and integrated into Zed:

1. **`list_history`** - List conversation history with stable indices
2. **`call_context_tool`** - Meta-tool that can dynamically invoke other context tools


## Current Status

### ✅ Completed

1. **Tool Implementation**
   - Both tools implemented in `crates/assistant_tools/src/context_management/`
   - Full input/output schemas with schemars support
   - Proper error handling and validation
   - Comprehensive UI text and descriptions

2. **Tool Registration**
   - Tools registered in `ToolRegistry` during `assistant_tools::init()`
   - Registration confirmed in startup logs:
     ```
     INFO  [assistant_tools] Registering context management tools: list_history, call_context_tool
     INFO  [assistant_tools] Registered ListHistoryTool
     INFO  [assistant_tools] (MemoryTool removed)
     INFO  [assistant_tools] Registered CallContextTool
     ```

3. **Default Profile Configuration**
   - Tools enabled by default in "write" profile (`assets/settings/default.json`)
   - Also enabled in "ask" profile
   - Configuration verified:
     ```json
     "profiles": {
       "write": {
         "tools": {
           "call_context_tool": true,
           "list_history": true,
           // "memory": true, (removed)
           ...
         }
       }
     }
     ```

4. **MCP Server Integration**
   - Embedded MCP server exposes tools for external access
   - Socket path written to `~/.zed/embedded_compaction_mcp_socket`
   - CLI tool (`agent_compaction_cli`) successfully connects and invokes tools
   - Tools tested via MCP protocol

5. **Documentation**
   - [MCP_TOOLS_IMPLEMENTATION.md](./MCP_TOOLS_IMPLEMENTATION.md) - Implementation details
   - [TESTING_MCP_TOOLS.md](./TESTING_MCP_TOOLS.md) - MCP/CLI testing guide
   - [MCP_ARCHITECTURE_CLARIFICATION.md](./MCP_ARCHITECTURE_CLARIFICATION.md) - Architecture overview
   - [TESTING_CONTEXT_TOOLS.md](./TESTING_CONTEXT_TOOLS.md) - Assistant UI testing guide

6. **Code Quality**
   - All compiler warnings resolved
   - Proper error propagation (no silent failures)
   - Logging at appropriate levels
   - Clean git history with atomic commits

### ⚠️ Known Limitations

1. **Thread Context Not Wired**
   - Tools are currently **stateless** - they don't have access to actual thread/conversation context
   - MCP tools return placeholder responses
   - Native tools need to be tested in live assistant sessions to verify they receive context

2. **Memory Storage**
   - (Removed feature) Memory tool and its in-memory storage have been deleted
   - —
   - —

3. **Session Management**
   - Session IDs are generated but not consistently passed through MCP layer
   - Need to wire session context from MCP client to tools

## Verification Checklist

### Build & Startup
- [x] Code compiles without errors
- [x] No compiler warnings
- [x] Tools registered at startup
- [x] MCP server initialized
- [x] Socket file created

### Configuration
- [x] Tools listed in default profile
- [x] Tools enabled by default
- [x] Profile settings load correctly

### MCP Server
- [x] Socket connection works
- [x] CLI can list tools
- [x] CLI can invoke tools
- [x] Tools return structured responses

### Native Tools (Needs Testing)
- [ ] Tools appear in assistant tool list
- [ ] Assistant can invoke tools
- [ ] Tools receive thread context
- [ ] Tools return real conversation data
- [ ] Tool output renders correctly in UI

## Next Steps

### Priority 1: Verify Native Tool Integration
1. Launch Zed with `cargo run`
2. Open assistant panel
3. Start a conversation
4. **Verify tools are listed** when the model can use them
5. **Test tool invocation** by asking assistant to use them
6. **Check tool output** appears correctly

### Priority 2: Wire Thread Context
1. Pass thread/session context to native tools
2. Implement message history access in `list_history`
3. Implement real archival in `memory` tool
4. Wire MCP tools to actual thread data

### Priority 3: Persistent Storage
1. Design database schema for archived memories
2. Implement SQLite backend
3. Add migration support
4. Test persistence across restarts

### Priority 4: Enhanced Features
1. Add search/filter to `list_history`
2. Add tags/labels to memories
3. Implement memory expiration/TTL
4. Add memory statistics and analytics

## Testing Instructions

### Test Native Tools in Assistant

1. Build and run Zed:
   ```bash
   cd ~/code/zed
   cargo run
   ```

2. Open assistant panel (Cmd/Ctrl+?)

3. Start a new conversation

4. Try these prompts:
   ```
   Can you list the conversation history?
   ```
   
   ```
   Can you archive the first 3 messages with the summary "test archive"?
   ```
   
   ```
   Can you list all archived memories?
   ```

5. Check logs for tool invocations:
   ```bash
   cargo run 2>&1 | grep -E "(list_history|memory|call_context_tool)"
   ```

### Test via MCP CLI

See [TESTING_MCP_TOOLS.md](./TESTING_MCP_TOOLS.md) for detailed CLI testing instructions.

## Architecture Notes

### Two Tool Systems

Zed has two parallel tool systems:

1. **Native ToolRegistry** (`assistant_tools` crate)
   - Used by Zed's internal agent/assistant
   - Tools implement the `Tool` trait
   - Registered during `assistant_tools::init()`
   - Enabled via agent profile settings

2. **Embedded MCP Server** (`context_server` crate)
   - Exposes tools to external clients via Unix socket
   - Tools implement the `McpServerTool` trait
   - Follows Model Context Protocol (MCP) specification
   - Used by CLI tools and other MCP clients

Both systems expose the same three context management tools, but they:
- Share the same input/output schemas
- Have separate implementations (native vs MCP)
- Serve different purposes (internal vs external)

### Key Files

- `crates/assistant_tools/src/assistant_tools.rs` - Tool registration
- `crates/assistant_tools/src/context_management/` - Tool implementations
- `crates/agent/src/embedded_mcp_server.rs` - MCP server setup
- `assets/settings/default.json` - Default tool configuration
- `crates/agent/src/agent_profile.rs` - Profile/tool enabling logic

## Troubleshooting

### Tools not appearing in assistant
1. Check logs for registration messages
2. Verify profile settings in `~/.config/zed/settings.json`
3. Ensure default profile is "write"
4. Check `AgentProfile::enabled_tools()` is called

### MCP connection errors
- Ignore "notifications/initialized" errors from enhanced-shell - these are benign
- Check socket file exists: `cat ~/.zed/embedded_compaction_mcp_socket`
- Verify socket is accessible: `ls -l $(cat ~/.zed/embedded_compaction_mcp_socket)`

### Tools return placeholder data
- This is expected for MCP tools until thread context is wired
- Native tools should have access to thread context automatically
- Check thread is passed correctly through tool invocation chain

## Related Issues

- Context management tools needed for conversation memory
- Agents need ability to manage long conversations
- External tools need access to conversation context

## Contributors

- Implementation: Torstein Sørnes
- Architecture guidance: Claude (Anthropic)

## References

- [Model Context Protocol Specification](https://modelcontextprotocol.io/)
- [Zed Agent Architecture](./docs/agent.md)
- [GPUI Tool System](./crates/assistant_tool/src/assistant_tool.rs)