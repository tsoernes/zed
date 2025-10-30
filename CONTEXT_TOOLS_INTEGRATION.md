# Context Management Tools Integration Guide

## Overview

This document explains how the context management tools (`list_history`) were implemented and integrated into Zed's LLM agent system. (The `memory` tool has been removed.)

## The Issue

The tools were successfully implemented and registered in the `ToolRegistry`, but they weren't available to the LLM agent. The startup logs showed:

```
INFO  [assistant_tools] Registering context management tools: list_history
INFO  [assistant_tools] Registered ListHistoryTool
INFO  [assistant_tools] (MemoryTool removed)
```

Despite successful registration, attempting to use these tools resulted in errors like:
```
No tool named 'list_history' exists
```

## Root Cause

Zed's agent system uses a **two-tier tool system**:

1. **ToolRegistry** (global): All tools are registered here at startup
2. **AgentProfile** (per-profile): Tools must be explicitly **enabled** in agent profile settings

The flow is:
```
ToolRegistry::register_tool() 
  ↓
AgentProfile::enabled_tools(cx)  ← Filters based on settings
  ↓
Thread::available_tools()  ← Builds LanguageModelRequestTool list
  ↓
LanguageModelRequest.tools  ← Sent to LLM
```

## The Solution

Tools must be added to the default agent profile settings in `assets/settings/default.json`:

```json
{
  "agent": {
    "profiles": {
      "write": {
        "tools": {
          "list_history": true,
          // "memory": true,  (removed)
          // ... other tools
        }
      },
      "ask": {
        "tools": {
          "list_history": true,
          // "memory": true,  (removed)
          // ... other tools
        }
      }
    }
  }
}
```

## Key Code Locations

### 1. Tool Registration
**File**: `crates/assistant_tools/src/assistant_tools.rs`
```rust
pub fn init(http_client: Arc<HttpClientWithUrl>, cx: &mut App) {
    let registry = ToolRegistry::global(cx);
    registry.register_tool(ListHistoryTool);
    // registry.register_tool(MemoryTool); // removed
    registry.register_tool(CallContextTool);
}
```

### 2. Profile Filtering
**File**: `crates/agent/src/agent_profile.rs`
```rust
pub fn enabled_tools(&self, cx: &App) -> Vec<(UniqueToolName, Arc<dyn Tool>)> {
    let Some(settings) = AgentSettings::get_global(cx).profiles.get(&self.id) else {
        return Vec::new();
    };

    self.tool_set
        .read(cx)
        .tools(cx)
        .into_iter()
        .filter(|(_, tool)| Self::is_enabled(settings, tool.source(), tool.name()))
        .collect()
}

fn is_enabled(settings: &AgentProfileSettings, source: ToolSource, name: String) -> bool {
    match source {
        ToolSource::Native => *settings.tools.get(name.as_str()).unwrap_or(&false),
        // ...
    }
}
```

### 3. Request Building
**File**: `crates/agent/src/thread.rs`
```rust
pub fn available_tools(&self, cx: &App, model: Arc<dyn LanguageModel>) -> Vec<LanguageModelRequestTool> {
    if model.supports_tools() {
        self.profile
            .enabled_tools(cx)  // ← Gets filtered list from profile
            .into_iter()
            .filter_map(|(name, tool)| {
                let input_schema = tool.input_schema(model.tool_input_format()).ok()?;
                Some(LanguageModelRequestTool {
                    name: name.into(),
                    description: tool.description(),
                    input_schema,
                })
            })
            .collect()
    } else {
        Vec::default()
    }
}
```

## Tool Architecture

### Native Tools vs Context Server Tools

- **Native Tools** (`ToolSource::Native`): Built into Zed, registered in `assistant_tools::init()`
- **Context Server Tools** (`ToolSource::ContextServer`): Provided by external MCP servers

Both types must be enabled in profile settings, but context servers can use `enable_all_context_servers: true` as a shortcut.

### Profile Settings Structure

```rust
pub struct AgentProfileSettings {
    pub name: SharedString,
    pub tools: IndexMap<Arc<str>, bool>,  // ← Tool name → enabled
    pub enable_all_context_servers: bool,
    pub context_servers: IndexMap<Arc<str>, ContextServerPreset>,
}
```

## Debugging Tools

To verify tool registration and availability:

1. **Check registration logs** at startup:
   ```
   INFO  [assistant_tools] Registered ListHistoryTool
   ```

2. **Add debug logging** to `name()` methods:
   ```rust
   fn name(&self) -> String {
       log::info!("{}::name() called", stringify!(YourTool));
       "your_tool".to_string()
   }
   ```

3. **Verify profile settings** are loaded correctly

4. **Check `available_tools()` output** in the Thread

## Adding New Tools Checklist

When adding a new tool to Zed:

- [ ] Implement the `Tool` trait
- [ ] Register in `assistant_tools::init()`
- [ ] Add to default profiles in `assets/settings/default.json`
- [ ] Consider which profiles should have the tool enabled
- [ ] Test with `cargo build` and manual verification
- [ ] Document the tool's purpose and usage

## Context Management Tools

### `list_history`
Lists conversation history with message indices and previews.

### `memory`
Archives, loads, lists, restores, and prunes conversation segments for long-term context management.



## References

- Tool registration: `crates/assistant_tools/src/assistant_tools.rs`
- Profile settings: `crates/agent_settings/src/agent_profile.rs`
- Default settings: `assets/settings/default.json`
- Thread/request building: `crates/agent/src/thread.rs`
