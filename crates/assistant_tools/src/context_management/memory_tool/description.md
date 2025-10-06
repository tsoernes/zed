# Memory Tool

Archive, load, list, restore, or prune conversation message segments to manage context window usage.

## Purpose

The memory tool provides comprehensive management of conversation context by allowing you to:
- Archive contiguous ranges of messages to free up context window space
- Load archived content for inspection without modifying the conversation
- List all memory placeholders in the current conversation
- Restore archived messages back into the conversation
- Prune unused archived memories

## Operations

### Store

Archive a contiguous range of messages, replacing them with a compact placeholder.

**Required Parameters:**
- `operation`: "store"
- `start_index`: Inclusive starting message index
- `end_index`: Inclusive ending message index

**Optional Parameters:**
- `summary`: User-defined summary of the archived content
- `auto`: If true and summary is omitted, generates a heuristic summary (default: false)
- `max_preview_chars`: Character limit for preview (default: 200, clamped 40-400)

**Example:**
```json
{
  "operation": "store",
  "start_index": 5,
  "end_index": 30,
  "auto": true,
  "max_preview_chars": 200
}
```

**Output:** Creates a memory handle (e.g., `mem://session-id/uuid`) and replaces the message range with a placeholder containing the handle, summary, and preview.

### Load

Retrieve the full original content of an archived memory for inspection.

**Required Parameters:**
- `operation`: "load"
- `memory_handle`: The handle returned from a store operation

**Example:**
```json
{
  "operation": "load",
  "memory_handle": "mem://session-123/abc-def-456"
}
```

**Output:** Full markdown content of all archived messages with their roles and indices.

### List

Scan the current conversation thread for memory placeholders.

**Required Parameters:**
- `operation`: "list"

**Example:**
```json
{
  "operation": "list"
}
```

**Output:** A list of all memory placeholders found in the conversation, showing their indices, handles, and metadata.

### Restore

Insert archived messages back into the conversation.

**Required Parameters:**
- `operation`: "restore"
- `memory_handle`: The handle of the memory to restore

**Optional Parameters:**
- `restore_insert_index`: Target position for insertion (default: append to end)
- `remove_placeholder`: If true, removes the placeholder from its original location (default: false)
- `replace_placeholder_with`: Text to replace the placeholder with

**Example:**
```json
{
  "operation": "restore",
  "memory_handle": "mem://session-123/abc-def-456",
  "restore_insert_index": 100,
  "remove_placeholder": true
}
```

**Output:** Confirmation of restoration with message count and insertion position.

### Prune

Remove archived memories that no longer have corresponding placeholders in the conversation.

**Required Parameters:**
- `operation`: "prune"

**Example:**
```json
{
  "operation": "prune"
}
```

**Output:** Report of pruning operation and remaining memory count.

## Memory Placeholder Format

When messages are archived, they are replaced with a structured placeholder:

```
[[memory archived handle=mem://session-id/uuid range=5..30 messages=26 chars=12450]]
Summary: Initial project setup discussion and requirements gathering (26 msgs)
Preview: User: I need help setting up a new React project...
```

## Typical Workflow

1. Use `list_history` to inspect conversation messages
2. Identify a range of older messages that can be archived
3. Call `memory` with operation "store" to archive them
4. Continue conversation with freed context space
5. If needed, use `memory` with operation "load" to inspect archived content
6. Use `memory` with operation "restore" if archived information becomes relevant again
7. Periodically use `memory` with operation "prune" to clean up unused archives

## When to Use

- **Store**: When approaching token limits (60-70% of context window)
- **Load**: To review what was archived without restoring it to the conversation
- **List**: To see what memories are currently archived
- **Restore**: When archived information becomes relevant to the current discussion
- **Prune**: To clean up memory storage after a long conversation

## Notes

- Archives are stored in-memory for the session duration
- Memory handles are unique per session and UUID
- Archived content can be safely restored multiple times
- The heuristic summary uses the first non-empty message line (up to 140 chars)
- Previews are truncated with "...(truncated)" if they exceed max_preview_chars