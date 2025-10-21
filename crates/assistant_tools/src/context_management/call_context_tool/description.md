# Call Context Tool

A unified entry point for context management tools, providing dynamic invocation of either `list_history` or `memory` tools using a generic schema.

## Purpose

This multiplexer tool provides a single interface for accessing multiple context management capabilities. It's designed for environments where having fewer registered tools is preferable, while still supporting the full functionality of both `list_history` and `memory` tools.

## Parameters

- `name`: The name of the context tool to invoke ("list_history" or "memory")
- `arguments`: JSON object containing the arguments for the specified tool

## Supported Tools

### list_history

Enumerate conversation messages with indices and previews.

**Example:**
```json
{
  "name": "list_history",
  "arguments": {
    "start": 0,
    "limit": 40,
    "max_chars_per_message": 160,
    "include_full_markdown": false
  }
}
```

If `arguments` is omitted, defaults to an empty object (using tool defaults).

### memory

Archive, load, list, restore, or prune conversation segments.

**Example (Store):**
<!-- Legacy memory examples removed because the memory tool was deleted. -->

**Example (Load):**
```json
{
  "name": "memory",
  "arguments": {
    "operation": "load",
    "memory_handle": "mem://session-123/abc-def-456"
  }
}
```

**Note:** The `memory` tool requires the `arguments` field with at least an `operation` specified.

## Behavior

1. Validates the tool name
2. For `list_history`: Substitutes empty object if arguments are omitted
3. For `memory`: Requires arguments that include the `operation` field
4. Delegates execution to the appropriate underlying tool
5. Returns the same output as if the tool was called directly

## When to Use

- In environments that prefer fewer registered tools
- When you want a unified interface for context management
- For forward compatibility with future context management tools

## vs. Direct Tool Usage

You can use either:
- `call_context_tool` with `name` and `arguments` parameters
- Direct calls to `list_history` or `memory` tools

Both approaches provide identical functionality. The direct approach is simpler when the individual tools are available.

## Future Extensions

The multiplexer design allows additional context management tools to be added under the same unified interface without requiring changes to tool registration in calling environments.