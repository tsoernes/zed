# Chat History Tool Usage Guide

## Overview

The `ctx_chat_history` tool provides persistent storage, semantic search, and RAG (Retrieval-Augmented Generation) capabilities for assistant conversations. It enables finding similar past conversations, searching through chat history, and synthesizing answers from previous interactions.

## Status

**Current Status**: ✅ Tool implementation complete, ⚠️ Backend initialization required

The tool is fully implemented and available, but requires the chat history backend to be initialized in Zed. The backend includes:
- SQLite database for chat storage
- Embedding system (FastEmbed/OpenAI/Azure)
- BM25 index for keyword search
- Vector similarity search

## Tool Format

The `ctx_chat_history` tool uses a **tagged union** format where each operation is represented as an object with the operation name as the key.

### General Pattern
```json
{
  "operation_name": {
    "field1": "value1",
    "field2": "value2"
  }
}
```

## Operations Reference

### 1. Find Similar Chats (`similar`)

Find chats semantically similar to the current conversation or a specific chat.

**Key Feature**: When `chat_id` is omitted, automatically uses the current conversation's `thread_id`.

#### Examples

Find chats similar to the current conversation:
```json
{"similar": {"n": 10, "project_scoped": true}}
```

Find chats similar to a specific chat:
```json
{"similar": {"chat_id": "abc123", "n": 5, "project_scoped": false}}
```

Minimal (use all defaults):
```json
{"similar": {}}
```

#### Parameters
- `chat_id` (optional): Chat to find similarities for. Defaults to current conversation's thread_id.
- `n` (optional): Number of similar chats to return. Default: 10.
- `project_scoped` (optional): Limit to current project. Default: true.

#### Response Format
```json
{
  "ok": true,
  "similar": [
    {
      "chat": {
        "chat_id": "xyz789",
        "title": "Implementing async handlers",
        "summary": "Discussion about async/await patterns...",
        "total_messages": 15,
        "tags": ["rust", "async"]
      },
      "score": 0.87
    }
  ]
}
```

### 2. Search Messages (`search`)

Keyword and semantic search across all chat messages using hybrid BM25 + embedding retrieval.

#### Examples

Basic search:
```json
{"search": {"query": "rust async await patterns"}}
```

Search with mode control:
```json
{"search": {
  "query": "error handling",
  "mode": "hybrid",
  "top_k": 20,
  "alpha": 0.6
}}
```

Search within a project:
```json
{"search": {
  "query": "database migration",
  "project_id": "my-project",
  "top_k": 10
}}
```

Search within a specific chat:
```json
{"search": {
  "query": "implementation details",
  "chat_id": "abc123"
}}
```

#### Parameters
- `query` (required): Search query text
- `project_id` (optional): Limit to specific project
- `chat_id` (optional): Limit to specific chat
- `top_k` (optional): Max results to return
- `mode` (optional): "bm25" (keyword), "embedding" (semantic), "hybrid" (both). Default: "hybrid"
- `alpha` (optional): Fusion weight 0.0-1.0 (0=keyword, 1=semantic). Default: 0.55

#### Response Format
```json
{
  "ok": true,
  "contexts": [
    {
      "chat_id": "abc123",
      "message_id": "msg456",
      "role": "Assistant",
      "content_excerpt": "Here's how to implement async error handling...",
      "score": 0.92
    }
  ]
}
```

### 3. Answer Question (`answer`)

RAG synthesis - retrieve relevant context from chat history and generate an answer with citations.

#### Examples

Basic question:
```json
{"answer": {"question": "How did I implement error handling in the last project?"}}
```

Project-scoped question:
```json
{"answer": {
  "question": "What approach did we take for database migrations?",
  "project_id": "backend-rewrite"
}}
```

Question with retrieval tuning:
```json
{"answer": {
  "question": "Explain the async pattern we used",
  "top_k": 8,
  "mode": "embedding"
}}
```

#### Parameters
- `question` (required): Natural language question
- `project_id` (optional): Scope to specific project
- `chat_id` (optional): Scope to specific chat
- `top_k` (optional): Number of context chunks to retrieve
- `mode` (optional): Retrieval mode (see search)
- `alpha` (optional): Fusion weight (see search)

#### Response Format
```json
{
  "ok": true,
  "answer": "Based on previous discussions [1][2], you implemented error handling using the Result type with custom error enums..."
}
```

### 4. List Chats (`list`)

List stored chat metadata with pagination.

#### Examples

List recent chats:
```json
{"list": {"limit": 20, "offset": 0}}
```

List project-specific chats:
```json
{"list": {"project_id": "my-project", "limit": 50}}
```

Minimal (use defaults):
```json
{"list": {}}
```

#### Parameters
- `project_id` (optional): Filter by project
- `limit` (optional): Max chats to return. Default: 20
- `offset` (optional): Pagination offset. Default: 0

#### Response Format
```json
{
  "ok": true,
  "chats": [
    {
      "chat_id": "abc123",
      "project_id": "zed",
      "title": "Implementing chat history",
      "summary": "Discussion about chat history feature...",
      "total_messages": 42,
      "total_characters": 15234,
      "archived": false,
      "pinned": true,
      "tags": ["feature", "database"]
    }
  ]
}
```

### 5. Get Chat (`get`)

Retrieve full chat including all messages and metadata.

#### Example
```json
{"get": {"chat_id": "abc123"}}
```

#### Parameters
- `chat_id` (required): Chat identifier

#### Response Format
```json
{
  "ok": true,
  "chat": {
    "chat_id": "abc123",
    "title": "Implementing feature X",
    "summary": "...",
    "total_messages": 20
  },
  "messages": [
    {
      "id": "msg1",
      "chat_id": "abc123",
      "role": "User",
      "content": "How do I implement X?",
      "created_at": "2025-01-15T10:30:00Z",
      "token_estimate": 15
    }
  ]
}
```

### 6. Create Chat (`create_chat`)

Create a new empty chat session.

#### Examples

Create with title:
```json
{"create_chat": {"title": "New Feature Discussion", "project_id": "my-project"}}
```

Minimal:
```json
{"create_chat": {}}
```

#### Parameters
- `project_id` (optional): Associate with project
- `title` (optional): Chat title

#### Response Format
```json
{
  "ok": true,
  "chat": {
    "chat_id": "new-chat-id",
    "title": "New Feature Discussion",
    "created_at": "2025-01-15T12:00:00Z"
  }
}
```

### 7. Append Message (`append`)

Add a message to a chat. Auto-creates chat if `chat_id` is not provided.

#### Examples

Append to existing chat:
```json
{"append": {
  "chat_id": "abc123",
  "content": "This is helpful information",
  "role": "User"
}}
```

Create new chat with first message:
```json
{"append": {
  "content": "Starting a new conversation",
  "title": "New Topic",
  "project_id": "my-project"
}}
```

#### Parameters
- `chat_id` (optional): Target chat (creates new if omitted)
- `project_id` (optional): Project for new chat
- `title` (optional): Title for new chat
- `role` (optional): "User", "Assistant", "System", "Tool". Default: "User"
- `content` (required): Message text

#### Response Format
```json
{
  "ok": true,
  "chat": {
    "chat_id": "abc123",
    "total_messages": 5
  },
  "message": {
    "id": "msg123",
    "content": "This is helpful information",
    "created_at": "2025-01-15T12:05:00Z"
  }
}
```

### 8. Delete Chat (`delete_chat`)

Permanently delete a chat and all its messages.

#### Example
```json
{"delete_chat": {"chat_id": "abc123"}}
```

#### Parameters
- `chat_id` (required): Chat to delete

#### Response Format
```json
{
  "ok": true,
  "deleted": true
}
```

### 9. Update Metadata (`update_metadata`)

Update chat metadata: title, summary, tags, archived/pinned status.

**Requires confirmation** - user will be prompted before execution.

#### Examples

Update title and summary:
```json
{"update_metadata": {
  "chat_id": "abc123",
  "title": "Updated Title",
  "summary": "New summary text"
}}
```

Manage tags:
```json
{"update_metadata": {
  "chat_id": "abc123",
  "tags_add": ["important", "follow-up"],
  "tags_remove": ["draft"]
}}
```

Archive chat:
```json
{"update_metadata": {
  "chat_id": "abc123",
  "archived": true
}}
```

#### Parameters
- `chat_id` (required): Target chat
- `title` (optional): New title
- `summary` (optional): New summary
- `tags_add` (optional): Tags to add
- `tags_remove` (optional): Tags to remove
- `archived` (optional): Archive status
- `pinned` (optional): Pin status

#### Response Format
```json
{
  "ok": true,
  "chat": {
    "chat_id": "abc123",
    "title": "Updated Title",
    "tags": ["important", "follow-up"],
    "archived": true
  }
}
```

### 10. Recompute Embeddings (`reembed`)

Recompute embeddings for all chats or a specific chat. Useful after changing embedding model or fixing corrupted embeddings.

**Requires confirmation** - this is a potentially expensive operation.

#### Examples

Reembed all chats:
```json
{"reembed": {}}
```

Reembed specific chat:
```json
{"reembed": {"chat_id": "abc123"}}
```

#### Parameters
- `chat_id` (optional): Specific chat to reembed. Omit to reembed all.

#### Response Format
```json
{
  "ok": true,
  "status": "scheduled"
}
```

### 11. Get Configuration (`config_get`)

Retrieve current configuration (secrets redacted).

#### Example
```json
{"config_get": {}}
```

This is a unit-like variant that takes no parameters (but still requires the empty object).

#### Response Format
```json
{
  "ok": true,
  "config": {
    "default_retrieval_mode": "Hybrid",
    "embedding_backend": "FastEmbedLocal",
    "embedding_model": "bge-base-en-v1.5",
    "hybrid_alpha": 0.55,
    "similar_chats_k": 10,
    "summary_refresh_chars": 4000,
    "rag_top_k": 6,
    "auto_tag": true
  }
}
```

### 12. Set Configuration (`config_set`)

Update configuration parameters.

**Requires confirmation** - changes affect all future operations.

#### Examples

Change embedding model:
```json
{"config_set": {"embedding_model": "bge-large-en-v1.5"}}
```

Adjust hybrid fusion:
```json
{"config_set": {"hybrid_alpha": 0.7, "similar_chats_k": 15}}
```

Change retrieval defaults:
```json
{"config_set": {
  "default_retrieval_mode": "embedding",
  "rag_top_k": 10
}}
```

#### Parameters (all optional)
- `embedding_model`: Model name
- `hybrid_alpha`: Default fusion weight (0.0-1.0)
- `similar_chats_k`: Default number of similar chats
- `summary_refresh_chars`: Character threshold for summary refresh
- `summary_delta_chars`: Delta for summary updates
- `rag_top_k`: Default top-k for RAG retrieval
- `auto_tag`: Enable automatic tagging
- `default_retrieval_mode`: "bm25", "embedding", or "hybrid"

#### Response Format
```json
{
  "ok": true,
  "config": {
    "embedding_model": "bge-large-en-v1.5",
    "hybrid_alpha": 0.7
  }
}
```

## Common Use Cases

### Finding Related Conversations

To find conversations related to what you're currently discussing:
```json
{"similar": {"n": 5}}
```

This automatically uses the current conversation's context to find similar past chats.

### Answering "How Did I Do This Before?"

```json
{"answer": {"question": "How did I implement authentication in the previous project?"}}
```

Returns a synthesized answer with citations from your chat history.

### Searching for Specific Topics

```json
{"search": {"query": "database migration strategy", "mode": "hybrid", "top_k": 10}}
```

Combines keyword matching (BM25) with semantic similarity for best results.

### Reviewing Project History

```json
{"list": {"project_id": "backend-api", "limit": 50}}
```

Lists all chats associated with a specific project.

## Error Handling

All operations return JSON with an `"ok"` field:

### Success
```json
{"ok": true, "result": "..."}
```

### Failure
```json
{"ok": false, "error": "error message describing what went wrong"}
```

Common errors:
- `"chat history adapter not installed"` - Backend not initialized
- `"chat_id required or current conversation must have thread_id"` - Missing required parameter
- `"parse error: ..."` - Invalid JSON format
- `"chat not found"` - Referenced chat doesn't exist

## Backend Initialization

The chat history backend must be initialized in Zed for the tool to work. This involves:

1. Database setup (SQLite)
2. Embedding backend configuration (FastEmbed by default)
3. Index initialization (BM25 + vector similarity)
4. Chat history adapter registration

Check initialization status:
```json
{"config_get": {}}
```

If you get `"chat history adapter not installed"`, the backend needs to be initialized in the Zed codebase.

## Performance Considerations

- **Embeddings**: First message in a chat triggers embedding computation (async)
- **Search**: Hybrid mode is slightly slower than pure BM25 or pure embedding
- **Similar**: Requires chat embedding to exist; computes on-demand if missing
- **Reembed**: Expensive operation; recomputes all embeddings from scratch

## Tips for Best Results

1. **Use `similar` without `chat_id`** to find related conversations automatically
2. **Use `hybrid` mode** for search/answer - balances keyword and semantic matching
3. **Adjust `alpha`** if results are too keyword-focused (increase) or too broad (decrease)
4. **Use project scoping** to focus results on relevant codebases
5. **Add tags** via `update_metadata` to organize and filter conversations
6. **Use `answer`** when you want synthesis; use `search` when you want raw matches

## Future Enhancements

Planned improvements:
- Streaming RAG answers
- Incremental embedding updates
- HNSW approximate nearest neighbor index
- Advanced score fusion (RRF)
- Summary auto-generation
- Auto-tagging with keyphrase extraction