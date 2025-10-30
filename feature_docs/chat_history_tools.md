zed/feature_docs/chat_history_tools.md#L1-400
# Chat History Tools Adapter Documentation

This document details the `chat_history_tools` adapter layer that exposes a deterministic, JSON‑based, snake_case tool surface over the chat history core store. It is intended for reconstruction on a fresh upstream codebase.

---

## 1. Purpose

Provide a thin, stateless (aside from holding a shared store handle) interface that an agent or LLM can invoke via structured JSON strings:
- Uniform request/response envelopes
- Clear error handling
- Isolation of parsing/validation from storage logic
- Safe concurrency (single mutex guarding the store; background tasks within store internals)

---

## 2. High-Level Architecture

```
+---------------------+          +------------------+
|  Tool Caller (LLM)  |  JSON    |  Tools Adapter   |
|  or Agent Runtime   |--------->| (chat_history)   |
+---------------------+          | - Parse Input    |
                                 | - Build Request  |
                                 | - Call Store     |
                                 | - Wrap Response  |
                                 +--------+---------+
                                          |
                                          | async & mutex-guarded calls
                                          v
                                 +------------------+
                                 |  ChatStore Core  |
                                 |  - Persistence   |
                                 |  - Embeddings    |
                                 |  - Search / RAG  |
                                 +------------------+
```

---

## 3. Adapter Components

### 3.1 Shared Handles

```
ChatHistoryHandles {
  store: Arc<Mutex<ChatStore>>,
  tools: Arc<ChatHistoryTools>,
}
```

### 3.2 ChatHistoryTools

Holds:
```
struct ChatHistoryTools {
  store: Arc<Mutex<ChatStore>>
}
```
Public async methods implementing the tool API.

### 3.3 Tool API Trait (Conceptual)

```
trait ChatHistoryToolApi {
  async fn chat_append(&self, input_json: &str) -> String;
  async fn chat_search(&self, input_json: &str) -> String;
  async fn chat_answer(&self, input_json: &str) -> String;
  async fn chat_similar(&self, input_json: &str) -> String;
  async fn chat_list(&self, input_json: &str) -> String;
  async fn chat_get(&self, input_json: &str) -> String;
  async fn chat_reembed(&self, input_json: &str) -> String;
  async fn chat_update_metadata(&self, input_json: &str) -> String;
  async fn chat_config_get(&self) -> String;
  async fn chat_config_set(&self, input_json: &str) -> String;
}
```

---

## 4. Initialization Flow

Two pathways:

1. Synchronous (in-memory only):
```
init_chat_history_tools(options: ChatHistoryInitOptions) -> ChatHistoryHandles
```

2. Asynchronous (optional DB + index rebuild):
```
init_chat_history_tools_async(options: ChatHistoryInitOptions) -> ChatHistoryHandles
```

Settings precedence:
1. Load default + settings files (`.zed/settings.json`)
2. Overlay explicit `ChatHistoryInitOptions` fields
3. Later mutations via `chat_config_set`

---

## 5. Requests & Responses

### 5.1 Envelope Conventions

- Success: object contains `"ok": true`
- Failure: object contains `"ok": false, "error": "<string>"`

### 5.2 Common Error Conditions

| Condition                          | Error String Example                       |
|-----------------------------------|--------------------------------------------|
| Invalid JSON                      | `parse error: expected value at line ...`  |
| Missing required field            | `missing field 'chat_id'` (serde)          |
| Store operation failure           | Store-provided error message               |
| Config update invalid (future)    | `invalid hybrid_alpha`                     |

### 5.3 Example Success

```
{
  "ok": true,
  "chat": { "chat_id": "c1", "title": "Design", ... },
  "message": { "message_id": "m1", "role": "User", "content": "Initial notes" }
}
```

### 5.4 Example Failure

```
{
  "ok": false,
  "error": "parse error: invalid type: null, expected string"
}
```

---

## 6. Operation Definitions

#### 6.1 chat_append

Creates or extends a chat.

Input:
```
{
  "chat_id": "optional-existing",
  "project_id": "optional",
  "title": "optional",
  "role": "User|Assistant",
  "content": "text"
}
```

Behavior:
- If `chat_id` omitted: new chat; `title` optional.
- Embedding + summary scheduled asynchronously in store.
- Returns updated metadata + single message object.

Output:
```
{
  "ok": true,
  "chat": { ... },
  "message": { ... }
}
```

#### 6.2 chat_search

Hybrid or single-mode message retrieval.

Input:
```
{
  "query": "text",
  "project_id": "optional",
  "chat_id": "optional",
  "top_k": 20,
  "mode": "Hybrid|Bm25|Embedding",
  "alpha": 0.55
}
```

Output:
```
{
  "ok": true,
  "contexts": [
    {
      "chat_id": "...",
      "message_id": "...",
      "score": 0.87,
      "content": "matched text fragment",
      "role": "User"
    }
  ]
}
```

#### 6.3 chat_answer

RAG answer path (retrieval + model call abstraction).

Input similar to search but uses `question` key:
```
{
  "question": "What did we decide about latency?",
  "project_id": "...?",
  "chat_id": "...?",
  "top_k": 6,
  "mode": "Hybrid",
  "alpha": 0.6
}
```

Output (current minimal form):
```
{ "ok": true, "answer": "We agreed to batch embeddings to reduce latency." }
```
(Future extension may include `contexts` and `citations`.)

#### 6.4 chat_similar

Whole-chat embedding similarity.

Input:
```
{ "chat_id": "c7", "n": 10, "project_scoped": true }
```

Output:
```
{
  "ok": true,
  "similar": [
    { "chat": { "chat_id": "c3", ... }, "score": 0.91 }
  ]
}
```

#### 6.5 chat_list

Paginated metadata projection.

Input:
```
{ "project_id": "optional", "limit": 20, "offset": 0 }
```

Output:
```
{
  "ok": true,
  "chats": [
    {
      "chat_id": "c1",
      "project_id": "proj-123",
      "title": "Design",
      "summary": null,
      "total_messages": 4,
      "total_characters": 1024,
      "archived": false,
      "pinned": false,
      "tags": ["design","latency"]
    }
  ]
}
```

#### 6.6 chat_get

Load full chat.

Input:
```
{ "chat_id": "c1" }
```

Output:
```
{
  "ok": true,
  "chat": { ...full metadata... },
  "messages": [ { "message_id": "...", "role": "...", "content": "...", ... } ]
}
```

#### 6.7 chat_reembed

Recompute embeddings for one or all chats.

Input: `{ "chat_id": "optional" }` or `{}`

Output: `{ "ok": true, "status": "scheduled" }`

#### 6.8 chat_update_metadata

Modify selective metadata / tags.

Input:
```
{
  "chat_id": "c1",
  "title": "New Title",
  "summary": "Condensed summary",
  "tags_add": ["performance"],
  "tags_remove": ["latency"],
  "archived": false,
  "pinned": true
}
```

Output:
```
{ "ok": true, "chat": { ...updated metadata... } }
```

#### 6.9 chat_config_get

Redacts secrets.
Output example:
```
{
  "ok": true,
  "config": {
    "embedding_model": "bge-base-en-v1.5",
    "hybrid_alpha": 0.55,
    "openai": { "api_key": "****", "model": "text-embedding-3-small", ... },
    "azure_openai": { "api_key": "****", ... }
  }
}
```

#### 6.10 chat_config_set

Partial overlay of config.

Input:
```
{ "hybrid_alpha": 0.6, "rag_top_k": 8 }
```

Output:
```
{ "ok": true, "config": { "hybrid_alpha": 0.6, "rag_top_k": 8, ... } }
```

---

## 7. Concurrency & Background Work

- All tool invocations lock the store briefly (parse → request build → store method invocation).
- Embedding, summarization, and index rebuild tasks run outside the mutex (spawned inside store).
- Re-entrant calls wait for lock; no priority scheduling.

Sequence (append + embed scheduling):

```
Caller -> ToolsAdapter.chat_append
        -> Parse JSON
        -> Acquire store lock
        -> store.tool_append(...)
             |-> persist message
             |-> queue embedding task (async)
        -> Release store lock
        -> Return JSON envelope
Async embedding task ->
        -> Compute vector
        -> Update message + chat pooled vector
        -> Log errors if any
```

---

## 8. Re-Embedding Lifecycle

```
Caller chat_reembed
   |
   v
Parse input -> Lock store -> Determine target set -> Schedule job -> Unlock -> Return "scheduled"
   |
   v (background)
Fetch messages -> Batch embed -> Update vectors -> Recompute chat pools -> (Optional) Log completion
```

---

## 9. Configuration Precedence Diagram

```
          +---------------------------+
          |   Default Config          |
          +-------------+-------------+
                        |
                        v
          +---------------------------+
          | Settings File (.zed/...)  |
          +-------------+-------------+
                        |
                        v
          +---------------------------+
          | Init Options Overrides    |
          +-------------+-------------+
                        |
                        v
          +---------------------------+
          | Runtime chat_config_set   |
          +---------------------------+
```

At each layer, only provided fields are overwritten (partial overlay).

---

## 10. Embedding Backend Selection

Input option: `backend: Option<EmbeddingBackendKind>`

Variants (conceptual):
```
EmbeddingBackendKind::FastEmbedLocal { model_path: Option<String> }
EmbeddingBackendKind::OpenAI { /* future: explicit fields */ }
EmbeddingBackendKind::AzureOpenAI { /* Azure specifics */ }
```

Selection pseudocode:
1. If backend explicit: use it.
2. Else use FastEmbedLocal (with effective model name resolved from settings or overrides).
3. For OpenAI/Azure, validate presence of required secrets before constructing backend; fail early if missing.

---

## 11. Security & Secret Redaction

- GET config redacts `api_key` by replacing with `****` if non-empty.
- SET config expects plain text keys; adapter does not store historical keys.
- Adapter never logs raw keys (avoid debug prints containing secrets).

---

## 12. Error Handling Philosophy

- Parse errors short-circuit before acquiring store (except if store needed for context — not required here).
- Store failures propagate unchanged into envelope; no string transformations except `ToString`.
- No panics; all unexpected states should produce an error envelope.

---

## 13. Implementation Reconstruction Guide

Step-by-step to rebuild adapter:

1. Define request structs with `#[derive(Deserialize)]`, `#[serde(rename_all = "snake_case")]`.
2. Define minimal projection struct `ChatSummary` with `Serialize`.
3. Implement helper functions:
   - `err_json(msg: impl ToString) -> String`
   - `ok_json(value: serde_json::Value) -> String` (inject `"ok": true`).
4. Implement trait with pattern:
   ```
   async fn op(&self, input_json: &str) -> String {
       let input: InputType = match serde_json::from_str(input_json) { ... };
       let store = self.store.lock().await;
       match store.<method>(...) .await {
           Ok(result) => ok_json(json!({...})),
           Err(e) => err_json(e),
       }
   }
   ```
5. Expose handles via init functions returning `Arc<Mutex<ChatStore>>` + `Arc<ChatHistoryTools>`.
6. Ensure any embedding or summarization tasks spawn without holding mutex (store internal detail).
7. Provide tests:
   - Append → Get → List
   - Search vs different modes
   - Config precedence and set/get
   - Re-embed scheduling success

---

## 14. JSON Schema Considerations

While strict schemas are not enforced, consistent use of:
- snake_case keys
- optional numeric tuning fields
- explicit role enumeration (`User` / `Assistant`)
- floats for weights (`alpha`)
- booleans for flags (`archived`, `pinned`, `auto_tag`)

To introduce strict validation later, one may:
- Derive `JsonSchema` for request/response types (e.g. via `schemars`)
- Publish schema documents for dynamic tool ingestion

---

## 15. Extensibility Points

| Area               | Path to Extend                                 |
|--------------------|------------------------------------------------|
| Additional search filters | Add fields to `SearchInput` (date range, role) |
| RAG context return  | Include `contexts` in `chat_answer` response     |
| Streaming answers   | Return an ID; subscribe to answer events         |
| Auth / ACL          | Wrap store calls with permission checks         |
| Secret rotation     | Add method `chat_config_rotate_keys`            |
| Batch operations    | Provide multi-chat append/list variants         |

---

## 16. Sequence Diagram: chat_answer

```
Caller
  |
  | JSON: {"question":"...","top_k":6,"mode":"Hybrid"}
  v
ToolsAdapter.chat_answer
  |
  | Parse -> Build request
  | Lock store
  v
ChatStore.tool_answer
  |
  | Retrieve messages (search/hybrid)
  | Rank & trim contexts
  | Construct prompt
  | Invoke model (outside scope)
  | Produce answer string
  v
ToolsAdapter
  |
  | Wrap in {"ok":true,"answer": "..."}
  v
Caller
```

---

## 17. Testing Strategy Summary

| Test Name                    | Purpose                                     |
|-----------------------------|---------------------------------------------|
| precedence_file_then_options | Confirms override layering correctness      |
| append_and_get               | Validates creation + retrieval              |
| search_hybrid_vs_embedding   | Ensures different modes yield differences   |
| similar_chats_scoping        | Project-scoped filtering correctness        |
| reembed_schedules            | Confirms scheduling doesn't panic           |
| config_set_redaction_get     | Redaction of secrets on GET                 |

All tests should avoid relying on external network calls unless keys configured; use FastEmbed backend by default for deterministic runs.

---

## 18. Performance Considerations

- Mutex granularity: coarse around store; keep store lock duration short.
- Batch embeddings: store groups messages internally (not adapter concern).
- Large list or search responses: consider pagination (already supported via `limit`/`offset`).
- Re-embedding cost: potentially heavy; marking operation as “scheduled” avoids waiting.

---

## 19. Failure & Recovery Scenarios

| Scenario                     | Handling                                |
|------------------------------|------------------------------------------|
| Embedding backend unavailable | Return error on first embed call        |
| Reembed with missing chat     | Error envelope                           |
| Invalid mode enum             | Serde parse failure -> error envelope    |
| Missing API key (OpenAI/Azure)| Init failure before adapter created      |
| DB connection lost (async init)| Future store operations error out       |

---

## 20. Migration / Versioning Plan (Suggested)

Introduce a top-level `version` field in config GET responses on evolution:
```
{ "ok": true, "version": 1, "config": { ... } }
```
Adapter can then branch logic for backward compatibility.

---

## 21. Example End-to-End Session

1. Initialization (FastEmbed default)
2. `chat_append` user message -> chat created
3. `chat_append` assistant response -> message added; embedding scheduled
4. `chat_search` query for “latency” -> contexts returned
5. `chat_answer` question “Summarize our latency decisions” -> answer returned
6. `chat_update_metadata` to pin chat
7. `chat_config_set` adjusts `hybrid_alpha` to 0.6
8. `chat_config_get` shows redacted keys and new alpha
9. `chat_reembed` after changing embedding model (if done) -> scheduled

---

## 22. Reconstruction Checklist (Condensed)

- Define request/response structs with snake_case serde.
- Implement adapter with uniform envelope creators.
- Lock store only around direct calls.
- Provide init functions merging settings + overrides.
- Ensure config GET redacts keys.
- Add tests for each endpoint’s primary success/failure path.

---

## 23. ASCII Class Diagram (Conceptual)

```
+-----------------------+
| ChatHistoryTools      |
|-----------------------|
| - store: Arc<Mutex<>> |
|-----------------------|
| + chat_append(...)    |
| + chat_search(...)    |
| + chat_answer(...)    |
| + chat_similar(...)   |
| + chat_list(...)      |
| + chat_get(...)       |
| + chat_reembed(...)   |
| + chat_update_metadata|
| + chat_config_get()   |
| + chat_config_set(...)|
+-----------+-----------+
            |
            v (lock)
+-----------------------+
| ChatStore             |
|-----------------------|
| + tool_append(...)    |
| + tool_search(...)    |
| + tool_answer(...)    |
| + similar_chats(...)  |
| + list_chats(...)     |
| + get_chat(...)       |
| + reembed(...)        |
| + update_metadata(...)|
| + config()/set_config |
+-----------------------+
```

---

## 24. Future Enhancements (Design Notes)

- Introduce streaming variant: `chat_answer_stream` returning an ID; incremental events via subscription or polling.
- Add `chat_delete` with soft-delete flag to preserve embeddings for analytics.
- Extend `chat_search` with time bounding: `from_ts` / `to_ts`.
- Implement partial re-embedding: only chats missing vectors for new model.

---

## 25. Summary

The adapter is intentionally minimal:
- One mutex-protected store handle
- Deterministic JSON envelopes
- Clear separation of parsing, store invocation, and response building
- Extensible configuration and backend selection
- Resilient error propagation (no silent failures)

Reimplementing it requires only standard async Rust primitives (futures, Arc, Mutex), serde for JSON, and adherence to naming conventions.

End of chat_history_tools documentation.