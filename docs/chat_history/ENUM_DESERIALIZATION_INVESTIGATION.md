# ChatHistoryOperation Enum Deserialization Investigation

## Date
January 15, 2025

## Problem Statement

The `ctx_chat_history` tool's `operation` parameter (of type `ChatHistoryOperation` enum) fails to deserialize when invoked through the MCP interface, despite:
- ✅ Tool compiles successfully
- ✅ Schema is properly defined
- ✅ Internally tagged serde format is applied
- ❌ Struct variants fail with deserialization errors

### Error Observed
```
invalid type: string "{\"type\": \"config_get\"}", expected internally tagged enum ChatHistoryOperation
```

**Key Observation**: The JSON is being received as a **string** (wrapped in quotes) instead of as a JSON object.

## Architecture Overview

### Tool Invocation Flow

```
Claude Desktop (MCP Client)
    ↓ (MCP Protocol)
Zed Agent (MCP Server)
    ↓ (ACP Protocol)
agent2::Thread
    ↓ (Tool Invocation)
agent2/src/thread.rs:2936 → serde_json::from_value(input)?
    ↓
AgentTool::run(input, ...)
    ↓
ChatHistoryAgentTool with ChatHistoryOperation enum
```

### Key Components

1. **ChatHistoryOperation Enum** (`assistant_tools/src/context_management/chat_history_tool.rs`)
   - Defined with `#[serde(tag = "type", rename_all = "snake_case")]`
   - 12 variants (Append, Search, Answer, Similar, List, etc.)
   - Expected format: `{"type": "list", "limit": 10}`

2. **ChatHistoryAgentTool** (`agent2/src/tools/chat_history_tool.rs`)
   - Implements `AgentTool` trait
   - Input type: `ChatHistoryAgentToolInput { operation: ChatHistoryOperation }`
   - Bridges to chat_history adapter

3. **Deserialization Site** (`agent2/src/thread.rs:2936`)
   ```rust
   fn run(self: Arc<Self>, input: serde_json::Value, ...) -> Task<...> {
       cx.spawn(async move |cx| {
           let input = serde_json::from_value(input)?;  // ← FAILS HERE
           // ...
       })
   }
   ```

## Investigation Findings

### 1. Schema vs. Enum Mismatch (FIXED)

**Issue**: The tool schema described a flat discriminated union, but the enum used external tagging.

**Solution Applied**: Changed to internally tagged enum:
```rust
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatHistoryOperation {
    Append { /* fields */ },
    ConfigGet {},  // Changed from unit variant to struct variant
    // ...
}
```

**Status**: ✅ Schema now matches enum structure

### 2. Unit vs. Struct Variants

**Discovery**: With internally tagged enums, all variants must be struct-like.
- Unit variant: `ConfigGet` → Doesn't work with `tag = "type"`
- Struct variant: `ConfigGet {}` → Compatible with internal tagging

**Changes Made**:
- `ConfigGet` → `ConfigGet {}`
- Updated all pattern matches: `ConfigGet` → `ConfigGet {}`

**Status**: ✅ All variants now compatible with internal tagging

### 3. String Wrapping Issue (UNRESOLVED)

**Problem**: The `input` parameter arrives as a JSON-encoded string rather than a parsed object.

**Evidence**:
- Error message: `invalid type: string "{\"type\": \"config_get\"}"`
- The outer quotes indicate string wrapping
- `serde_json::from_value` receives `Value::String` instead of `Value::Object`

**Hypothesis**: Parameter encoding happens at the protocol boundary (MCP → ACP → agent2)

### 4. Comparison with Working Tools

**Observation**: Simple tools work fine, suggesting the issue is specific to complex enums.

**Working Example** (`MemoryOperation`):
- Also an enum with struct variants
- Uses external tagging (no `tag` attribute)
- Experiences SAME issue: `unknown variant "{\"type\": \"stats\"}"`

**Implication**: This is a general problem with enum parameters in agent2 tools, not specific to `ChatHistoryOperation`.

### 5. Parameter Flow Analysis

**Where Parameters Come From**:

1. **LLM Tool Call** (Normal Flow):
   ```
   LLM (Claude) → Returns LanguageModelToolUse
   LanguageModelToolUse.input → Already a serde_json::Value
   thread.rs:2107 → tool.run(tool_use.input, ...)
   thread.rs:2936 → serde_json::from_value(input)?
   ```
   **Status**: This flow works! LLM-generated tool calls succeed.

2. **MCP Tool Call** (Test/Manual Flow):
   ```
   Claude Desktop (MCP Client) → Sends CallToolParams
   CallToolParams.arguments → Option<serde_json::Value>
   ??? → How does this reach agent2 tools?
   ```
   **Status**: This is where the string wrapping occurs.

**Key Finding**: The issue ONLY affects MCP-invoked tools, not LLM-invoked tools!

## Root Cause Analysis

### Confirmed Facts

1. ✅ Enum definition is correct (internally tagged)
2. ✅ Schema matches enum structure
3. ✅ LLM can successfully invoke the tool (when Claude generates tool calls)
4. ❌ MCP manual invocation fails with string-wrapped JSON
5. ❌ The wrapping happens before `serde_json::from_value`

### Likely Causes

**Hypothesis 1: MCP Parameter Serialization**
- MCP protocol passes `arguments` as JSON
- Somewhere in the chain, this JSON gets double-encoded as a string
- By the time it reaches `from_value`, it's `Value::String("{...}")`

**Hypothesis 2: Tool Schema Interpretation**
- MCP client (Claude Desktop) might serialize based on schema differently
- The schema says parameter is an object with "operation" field
- MCP might be serializing the entire object as a string

**Hypothesis 3: Parameter Type Mismatch**
- AgentTool expects typed input: `ChatHistoryAgentToolInput`
- MCP provides untyped JSON: `serde_json::Value`
- Bridging layer might be adding string encoding

## Proposed Solutions

### Option 1: Custom Deserializer (RECOMMENDED)

Add a custom deserializer that handles both formats:

```rust
impl<'de> Deserialize<'de> for ChatHistoryAgentToolInput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        
        // Try direct deserialization
        if let Ok(input) = serde_json::from_value::<Self>(value.clone()) {
            return Ok(input);
        }
        
        // Try unwrapping if it's a string
        if let Value::String(s) = value {
            if let Ok(parsed) = serde_json::from_str::<Self>(&s) {
                return Ok(parsed);
            }
        }
        
        Err(serde::de::Error::custom("Failed to deserialize ChatHistoryAgentToolInput"))
    }
}
```

**Pros**:
- Handles both formats transparently
- No changes to tool invocation code
- Backward compatible

**Cons**:
- Band-aid solution, doesn't fix root cause
- Needs to be applied to all affected tools

### Option 2: Fix at Protocol Layer

Investigate and fix the parameter encoding in the MCP → ACP bridge:

**Investigation Points**:
1. How does embedded MCP server receive tool calls?
2. How are MCP CallToolParams converted to agent2 tool inputs?
3. Is there a serialization wrapper being added?

**Files to Investigate**:
- `agent/src/embedded_mcp_server.rs` - MCP server implementation
- `context_server/src/listener.rs:276-286` - CallTool handling
- `acp_thread/src/connection.rs` - ACP connection bridge
- Bridge between MCP tools and agent2 tools (if exists)

**Pros**:
- Fixes root cause for all tools
- Cleaner solution

**Cons**:
- More invasive changes
- Requires deep protocol understanding

### Option 3: Flatten Parameter Structure

Instead of nested enum, use a flat structure:

```rust
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ChatHistoryAgentToolInput {
    #[serde(rename = "type")]
    pub operation_type: String,
    
    // All possible fields (optional)
    #[serde(flatten)]
    pub fields: HashMap<String, serde_json::Value>,
}
```

Then manually construct the enum in the tool implementation.

**Pros**:
- Simpler deserialization
- More forgiving of format variations

**Cons**:
- Loses type safety
- More manual validation needed
- Doesn't align with idiomatic Rust

### Option 4: Use String Parameters

Change agent2 tools to accept string inputs and parse manually:

```rust
impl AgentTool for ChatHistoryAgentTool {
    type Input = String;  // Instead of ChatHistoryAgentToolInput
    
    fn run(self: Arc<Self>, input: Self::Input, ...) -> Task<...> {
        // Parse JSON string manually
        let operation: ChatHistoryOperation = serde_json::from_str(&input)?;
        // ...
    }
}
```

**Pros**:
- Bypasses the deserialization layer
- Handles string-wrapped JSON naturally

**Cons**:
- Loses type checking
- Inconsistent with other tools
- Error messages less helpful

## Recommended Action Plan

### Phase 1: Quick Fix (Option 1)
1. ✅ Implement custom deserializer for `ChatHistoryAgentToolInput`
2. ✅ Test with MCP client
3. ✅ Apply to `MemoryOperation` if successful
4. Document workaround

### Phase 2: Root Cause Fix (Option 2)
1. Trace parameter flow from MCP to agent2
2. Identify where string wrapping occurs
3. Fix at the protocol boundary
4. Remove custom deserializers once root cause is fixed

### Phase 3: Testing
1. Test LLM-generated tool calls (should still work)
2. Test MCP direct tool calls (should now work)
3. Test with all operation variants
4. Add integration tests

## Testing Checklist

- [ ] ConfigGet (unit-like struct variant)
- [ ] List (struct variant with optional fields)
- [ ] Similar (struct variant with optional chat_id)
- [ ] Search (struct variant with required + optional fields)
- [ ] Answer (RAG synthesis)
- [ ] Append (mutation operation)
- [ ] All variants through LLM
- [ ] All variants through MCP direct call

## References

- Serde internally tagged enums: https://serde.rs/enum-representations.html#internally-tagged
- Agent2 tool system: `agent2/src/thread.rs:2803-2960`
- MCP protocol: `context_server/src/types.rs`
- ACP protocol: External crate `agent-client-protocol` v0.4.3

## Open Questions

1. Why does the MCP flow add string wrapping when the LLM flow doesn't?
2. Is there a bridge layer between MCP tools and agent2 tools?
3. How do other agent2 tools with enum parameters handle this?
4. Is this specific to Zed's MCP implementation or a general MCP issue?

## Conclusion

The `ChatHistoryOperation` enum structure is now correct with internally tagged serde format, but a parameter encoding issue in the MCP invocation path prevents successful deserialization. The issue manifests as JSON being wrapped in a string before reaching the deserializer.

**Short-term**: Implement custom deserializer to handle string-wrapped JSON.
**Long-term**: Fix parameter encoding at the MCP/ACP protocol boundary.

The improvements to schema documentation and enum structure are valuable regardless, as they ensure the LLM receives clear, accurate tool schemas for proper invocation.