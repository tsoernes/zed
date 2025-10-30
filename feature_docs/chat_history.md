# Chat History & Extended Agent2 Feature Documentation

This document describes the non‑mainline (feature branch) subsystems you have added or refactored:
- Chat History core (`chat_history` crate)
- Chat History tool adapter (`chat_history_tools` crate)
- Memory archival / message splitting (agent2 `Thread` memory segments)
- Enhanced Terminal tool (assistant tools integration)
- Context / message history persistence & retrieval (chat + thread)
- Global memory backend (context management / memory tools)
- Azure OpenAI (and OpenAI) embedding backend integration
- FastEmbed local embedding backend
- RAG / hybrid retrieval surfaces

It is written so a coding LLM can reconstruct the approximate implementation on a fresh upstream checkout. Where code is elided, explicit structural descriptions and invariants are provided.

---

## 1. Chat History Core (`chat_history`)

### 1.1 Goals

Provides persistent, queryable, semantically retrievable past chats to enrich new assistant turns:
- Append messages reliably (auto‑persist + metadata updates).
- Embed messages (local FastEmbed or remote OpenAI / Azure OpenAI).
- Compute pooled chat embeddings (mean pool of per‑message vectors).
- Perform hybrid search over past messages (BM25 lexical + embedding cosine).
- Surface similar chats (chat‑level embedding similarity).
- Offer RAG answer route that retrieves message contexts, constructs a capped prompt, returns answer + citations.
- Automatically refresh summaries when character thresholds exceeded.

### 1.2 Core Data Structures (Conceptual)

```
ChatMetadata {
  chat_id: String
  project_id: Option<String>
  title: Option<String>
  summary: Option<String>
  created_at: DateTime<Utc>
  updated_at: DateTime<Utc>
  total_messages: usize
  total_characters: usize
  token_estimate: usize
  embedding_model: String
  archived: bool
  pinned: bool
  tags: Vec<String>
  chat_vector: Option<Vec<f32>>     // mean pooled message vectors
}

ChatMessage {
  message_id: String
  chat_id: String
  role: MessageRole                 // User | Assistant
  content: String
  created_at: DateTime<Utc>
  token_estimate: usize
  embedding_digest: Option<String>  // hash(text) for caching
  vector: Option<Vec<f32>>          // may be stored separate; accessible via join or cache
}
```

### 1.3 Configuration (`ChatHistoryConfig`)

Fields (default values taken from crate defaults):
- `embedding_model: String` (e.g. `bge-base-en-v1.5`)
- `hybrid_alpha: f32` (weight for embedding vs lexical; default ~0.55)
- `similar_chats_k: usize` (default 10)
- `summary_refresh_chars: usize` (absolute size requiring full summary regeneration)
- `summary_delta_chars: usize` (minimum delta since last summary to trigger regeneration)
- `rag_top_k: usize` (default number of contexts for answer)
- `auto_tag: bool` (enable naive keyphrase/tag suggestion)
- `default_retrieval_mode: RetrievalMode` (Enum: `Bm25`, `Embedding`, `Hybrid`)
- Nested provider configs:
  - `openai { api_key?, api_url?, model? }`
  - `azure_openai { api_key?, endpoint?, api_version?, deployment?, embedding_model? }`

Load order:
1. Settings file(s) `.zed/settings.json`
2. Explicit initialization overrides (`ChatHistoryInitOptions`)
3. In‑memory modifications via config set tool (`chat_config_set`)

### 1.4 Embedding Backends

Trait (simplified):
```
EmbeddingBackend {
  fn model_name(&self) -> &str
  fn embed(&self, batch: &[String]) -> Result<Vec<Vec<f32>>>
}
```

Implementations:
- `FastEmbedBackend::new(model_name)`
  - Local inference (e.g. BGE base); loads model once; streaming not required.
- `OpenAIEmbeddingBackend::new(model_name)`
  - Requires API key & URL presence (validated before use).
- `AzureOpenAIEmbeddingBackend::new_with_config(embedding_model, endpoint, api_key, api_version, deployment)`
  - Uses Azure’s deployment names; embedding model may default to deployment if unspecified.

Digest caching:
- Each message content hashed to `embedding_digest`; identical content reuse previous vector (exact string match).

Pooling for chat:
- Collect available message vectors; mean across dimension.
- Omitted messages (no vectors yet) reduce denominator; chat_vector only set after at least one vector.

### 1.5 Hybrid Search

Score fusion:
```
final_score = alpha * cosine_similarity + (1 - alpha) * normalized_keyword_score
```
Keyword score: BM25 or variant; normalized via max score or percentile scaling (implementation detail).
Selector logic chooses retrieval path based on requested `RetrievalMode`.

### 1.6 Similar Chats

Process:
1. Ensure each chat has `chat_vector`.
2. Filter by same project if `project_scoped = true`.
3. Compute cosine similarity; collect top N.
4. Return `(ChatMetadata, score)` tuples.

### 1.7 RAG Answer

Pipeline:
1. Accept `question`, optional `project_id`, optional `chat_id` scope.
2. Retrieve candidate messages (mode: hybrid / embedding / bm25).
3. Rank + deduplicate (avoid same message ID twice).
4. Trim contexts to token budget (approx from `token_estimate`).
5. Construct answer prompt with inline citation markers (e.g. `[#1]` referencing message index in returned contexts).
6. Call upstream language model (not shown in adapter).
7. Return JSON with `answer` (string). (Extended version could add `contexts` array; adapter currently returns just answer.)

### 1.8 Summary Refresh Logic

Triggered when:
- `total_characters >= summary_refresh_chars` AND
- `(total_characters - last_summary_characters) >= summary_delta_chars`

Background task re‑computes `summary` (stored in metadata). Summarization model can differ from main embedding model.

### 1.9 Error Handling Principles

- All fallible operations use `Result<T>`; errors bubble to adapter.
- Adapter returns JSON `{ "ok": false, "error": "<message>" }`.
- Background tasks log errors; they do not panic.
- Missing configuration (e.g., API key) yields an initialization error rather than deferred runtime failure.

---

## 2. Chat History Tools Adapter (`chat_history_tools`)

### 2.1 Initialization

`ChatHistoryInitOptions`:
- Mirrors `ChatHistoryConfig` overrides plus optional DB connection & rebuild flag.
- Returns `ChatHistoryHandles { store: Arc<Mutex<ChatStore>>, tools: Arc<ChatHistoryTools> }`.

Two variants:
- `init_chat_history_tools(options)` synchronous (in‑memory only).
- `init_chat_history_tools_async(options)` asynchronous (supports DB + index rebuild).

### 2.2 Tool Operations (snake_case)

All methods accept either JSON string input or no input, return a JSON string.

1. `chat_append`
   - Input:
     ```
     {
       "chat_id": "optional-existing",
       "project_id": "optional",
       "title": "optional",
       "role": "User|Assistant",
       "content": "text"
     }
     ```
   - If `chat_id` absent, creates new chat.
   - Output: `{ "ok": true, "chat": <ChatMetadata>, "message": <ChatMessage> }`

2. `chat_search`
   - Input: `{ "query": "...", "project_id": "...?", "chat_id": "...?", "top_k": 20?, "mode": "Hybrid|Bm25|Embedding", "alpha": 0.55? }`
   - Output: `{ "ok": true, "contexts": [ { "chat_id": "...", "message_id": "...", "score": f32, "content": "...", ... } ] }`

3. `chat_answer`
   - Input similar to search but `question`.
   - Output: `{ "ok": true, "answer": "..." }`

4. `chat_similar`
   - Input: `{ "chat_id": "...", "n": 10?, "project_scoped": true? }`
   - Output: `{ "ok": true, "similar": [ { "chat": <ChatMetadata>, "score": f32 } ] }`

5. `chat_list`
   - Input: `{ "project_id": "...?", "limit": 20?, "offset": 0? }`
   - Output: `{ "ok": true, "chats": [ { "chat_id": "...", ... } ] }` (projection fields only)

6. `chat_get`
   - Input: `{ "chat_id": "..." }`
   - Output: `{ "ok": true, "chat": <ChatMetadata>, "messages": [ <ChatMessage>... ] }`

7. `chat_reembed`
   - Input: `{ "chat_id": "...?" }` (none = all chats)
   - Schedules re‑embedding.
   - Output: `{ "ok": true, "status": "scheduled" }`

8. `chat_update_metadata`
   - Input allows partial updates:
     ```
     {
       "chat_id": "...",
       "title": "...?",
       "summary": "...?",
       "tags_add": ["t1","t2"]?,
       "tags_remove": ["t3"]?,
       "archived": true?,
       "pinned": false?
     }
     ```
   - Output: `{ "ok": true, "chat": <Updated ChatMetadata> }`

9. `chat_config_get`
   - Output: `{ "ok": true, "config": { ... } }`
   - Secrets (`api_key`) redacted to `"****"` if present.

10. `chat_config_set`
    - Input: subset of config fields; merges over current config (no partial secret redaction for new values).
    - Output: `{ "ok": true, "config": { ... } }`

### 2.3 Error Envelope

Any parse error or store error:
```
{ "ok": false, "error": "parse error: <serde>" }
{ "ok": false, "error": "<store failure>" }
```

### 2.4 Idempotency & Concurrency

- Store wrapped in `Arc<Mutex<ChatStore>>`: all tool calls serialize.
- Long‑running embedding tasks happen outside the mutex (internally via background spawn).
- Re‑embedding is safe to schedule multiple times; later result overwrites vectors.

---

## 3. Memory Archival / Message Splitting (Agent2 Thread)

### 3.1 Purpose

Reduces prompt size by replacing contiguous older messages with a compact summary placeholder (“memory segment”), enabling:
- Token budget adherence.
- Faster retrieval of recent context.
- Optional reconstruction (future expansion) from archived metadata.

### 3.2 Data Structure

```
ThreadMemorySegment {
  id: u64
  start: usize                // start message index (inclusive)
  end: usize                  // end message index (inclusive)
  summary: SharedString
  message_char_count: usize
  message_count: usize
  stored_epoch_ms: u128
  placeholder_char_count: usize
  message_token_count: usize  // aggregate or estimate
}
```

### 3.3 API Surface (Simplified)

- `store_memory_segment(start, end, cx) -> Result<u64>`
  - Validations: start <= end, end < messages.len(), no overlap with existing segments.
  - Produces automatic summary.
- `store_memory_segment_with_summary(start, end, Some(summary), cx)`
  - Optional custom summary (trim, collapse whitespace, limit length).
- Overlap detection:
  - Rejects if any existing segment intersects new range.

### 3.4 Effects

- Original messages replaced by a single placeholder summary message.
- Counts (char / tokens) updated to reflect archived state.
- Future summarization / retrieval can treat segment summary as a compressed prior context.

### 3.5 Failure Modes

- Out of bounds indices
- Overlap with existing segment
- Empty or invalid custom summary (fallback to auto summary path)

---

## 4. Enhanced Terminal Tool

### 4.1 Role

Provides safer, structured terminal invocation within UI / agent environment:
- Non‑interactive command execution (guards against long-lived blocking processes).
- Job listing & status (list currently spawned tasks).
- Integration points for agent to reference output or track ephemeral sessions.

### 4.2 Key Features (Approximate)

- `list_jobs` or similar enumeration (shows running asynchronous tasks).
- Output snapshot retrieval (bounded history).
- Terminal creation via environment trait (used by `AcpThreadEnvironment`).

### 4.3 Constraints

- Disallows starting indefinite servers under tool call context.
- Encourages single‑shot commands with combined stdout+stderr capture.
- Potential future extension: resource quota enforcement (CPU / timeouts).

---

## 5. Context / Message History Persistence

Two layers:
1. **Chat History**: Long‑term, multi‑chat persistence & cross‑chat retrieval (see sections 1 & 2).
2. **Thread Persistence (Agent2)**:
   - Each interactive thread (session) can be reloaded via `open_thread(session_id, cx)` which restores messages and registers subscriptions.
   - Saving triggered by observing thread entity changes (`save_thread` invoked from subscription list in `register_session`).
   - Registration ensures telemetry + model selection logic finds session by ID (fix for “Session not found” issue).

### 5.1 Session Registration Flow (Agent2)

- On new thread creation (`new_thread`):
  - Create `Thread` entity (fresh UUID session id).
  - Assign optional title (derived from cwd).
  - Call `register_session(thread, cx)`:
    - Build `AcpThread` wrapper.
    - Subscribe to title & token usage updates.
    - Insert `Session { thread, acp_thread, subscriptions... }` into `sessions` map.
  - Log debug: “Registered new session: <id>”.

### 5.2 Telemetry & Turn Execution

- `run_turn(session_id, ...)` looks up session map; returns error if absent.
- After fix, newly created threads populate `sessions` immediately, eliminating intermittent “Session not found”.

---

## 6. Global Memory Backend (Context Management)

### 6.1 Purpose

Abstracts a global memory layer accessible by tools / agent for cross‑thread or cross‑chat context:
- Allows injecting a custom backend (e.g. vector store, semantic cache).
- Provides get / set / search operations (not fully shown; feature branch includes placeholder `set_backend`).

### 6.2 API Highlight

`GlobalMemoryBackend::set_backend(cx: &mut App, backend: Arc<dyn MemoryBackend>)`
- Currently unused (warning).
- Future path: enabling dynamic switching between memory engines without restart.

### 6.3 Integration Points

- Tools needing broader recall than per‑chat history can call memory backend (e.g. summarizing multi‑project patterns).
- Chat history retrieval could be unified under a shared memory interface later.

---

## 7. Azure OpenAI Support (Embedding Backend)

### 7.1 Initialization Parameters

```
AzureOpenAiConfig {
  api_key: Option<String>,
  endpoint: Option<String>,        // https://<resource>.openai.azure.com
  api_version: Option<String>,     // e.g. "2024-02-15-preview"
  deployment: Option<String>,      // Azure deployment name
  embedding_model: Option<String>, // Optional override distinct from 'deployment'
}
```

### 7.2 Backend Construction

`AzureOpenAIEmbeddingBackend::new_with_config(embedding_model, endpoint, api_key, api_version, deployment)`

Validation:
- All non‑optional fields must be Some(..) else error.
- `embedding_model` may default to `deployment` if unset.

### 7.3 Failure Handling

- Missing key/endpoint/deployment returns initialization `Err(anyhow!(...))`.
- Embedding call errors propagate to tool wrappers; adapter responds with `{ "ok": false, "error": "<message>" }`.

---

## 8. OpenAI Support (Embedding Backend)

Parallel design:
- Requires `openai.api_key`.
- Optional `api_url` (defaults to `https://api.openai.com/v1`).
- Model name required (default `text-embedding-3-small` if absent).
- Same trait contract as other backends.

---

## 9. FastEmbed Local Backend

- Lightweight local CPU embedding (BGE family).
- Provided model path override (if future interface extends `EmbeddingBackendKind::FastEmbedLocal { model_path }`).
- Used as default when no remote backend chosen or secrets missing.

---

## 10. Re‑Embedding Workflow

### 10.1 Trigger

- Manual: `chat_reembed` tool call (optional chat scope).
- Automatic: model change (if config embedding_model modified) – schedules full re‑embedding.

### 10.2 Process

1. Collect target chats / messages.
2. Batch textual contents through chosen backend.
3. Update per‑message vectors + recompute chat pooled vector.
4. Log completion (adapter returns “scheduled” immediately).

### 10.3 Idempotency

Running again overwrites vectors; no merging required.

---

## 11. Security & Secrets Handling

- Config GET redacts `api_key` fields by replacing value with `"****"`.
- Config SET expects caller to supply keys; no redaction on input side.
- In-memory representation holds plaintext; persist layer (if DB used) should store keys only in settings file (not in chat metadata).

---

## 12. Performance Notes

- Embedding batching reduces overhead (size heuristics: group contiguous pending messages).
- Mean pooling chosen for simplicity (O(n*d)).
- Hybrid search cost dominated by embedding similarity + lexical scoring:
  - Lexical index rebuild optionally performed at async initialization (`rebuild_index` flag).
- Re‑embedding large corpora advisable to run in background (non‑blocking UI).

---

## 13. Testing Approach (Representative)

Unit tests / integration tests target:
- Override precedence (settings file vs options) — verifies merging.
- Append + search + similar flow correctness (expected presence of message IDs).
- Configuration mutation via `chat_config_set` reflects in subsequent operations.
- Memory segment overlap rules (store fails on invalid ranges).

Edge tests:
- Appending zero‑length content (should skip embedding, still persist message).
- Re‑embedding with no messages (should no‑op gracefully).
- Switching backend without clearing existing vectors (vectors remain; future diff may re‑embed automatically).

---

## 14. Known Limitations & Future Work

| Area | Current Behavior | Planned Enhancement |
|------|------------------|---------------------|
| Summaries | Character threshold based | Token & semantic change heuristics |
| ANN Index | Linear cosine over all chats | HNSW / IVF for scale >10k |
| Auto Tags | Basic keyphrase extraction | Weighted TF-IDF + embedding cluster labels |
| Memory Backend | Unused `set_backend` | Dynamic backend swapping & persistence |
| RAG Answer | Returns final answer only | Include contexts, citations metadata |
| Numeric Mentions | Stripped at UI stream layer | Configurable filters (allow/deny) |

---

## 15. Implementation Reconstruction Checklist

A fresh upstream checkout (main) lacks these crates/features. To recreate:

1. Create `chat_history` crate:
   - Define models & `ChatHistoryConfig`.
   - Implement `EmbeddingBackend` trait + FastEmbed backend.
   - Stub or implement OpenAI / Azure backends (with validation).
   - Build `ChatStore` with:
     - In‑memory metadata storage (HashMaps).
     - Optional DB handle (feature flag or trait).
     - Methods: append, search (bm25 + embedding), similar, answer, reembed, list, get, update_metadata, config getters/setters.
   - Summary scheduling (background task using thresholds).
   - Digest hashing (e.g. SHA256 of UTF‑8 content).

2. Create `chat_history_tools` crate:
   - Tool adapter struct holding `Arc<Mutex<ChatStore>>`.
   - JSON request structs (`serde` with `rename_all = "snake_case"`).
   - Implement trait with each tool method parsing JSON, constructing request, calling store, mapping result to envelope.

3. Extend agent2 `Thread`:
   - Add memory segment struct & vector.
   - Add `store_memory_segment(_with_summary)` with validations + summary creation.
   - Replace messages in the specified range with a placeholder summary message.

4. Fix session registration:
   - In connection `new_thread`, create `Thread` first, call `register_session`, log debug.

5. Enhanced terminal:
   - Provide a tool or API that executes non‑interactive commands; capture output; implement listing via stored task handles.

6. Global memory backend placeholder:
   - Interface trait (e.g. `MemoryBackend`) with search/put semantics.
   - `GlobalMemoryBackend::set_backend` storing static or contextual Arc.

7. Embedding backends:
   - Implement constructors & error handling aligning with documented config fields.
   - Provide `EmbeddingBackendKind` enum to select backend in init options.

8. Configuration precedence:
   - Loader reading `.zed/settings.json` -> merge overrides -> runtime config set.

9. RAG answer:
   - Retrieve top contexts (embedding / hybrid).
   - Concatenate context snippets into prompt template (include truncated content if needed).
   - Call external model (stub returning placeholder if model unavailable).
   - Return answer string in envelope.

10. Testing:
   - Precedence test (file then options).
   - Append/search/answer cycle ensures no panic & valid “ok”: true envelopes.
   - Memory segment overlap test.

---

## 16. JSON Examples

Append (new chat):
```
{"project_id":"proj-123","role":"User","content":"Initial design notes"}
```
Result:
```
{
  "ok": true,
  "chat": { "chat_id":"c1", "total_messages":1, ... },
  "message": { "message_id":"m1", "content":"Initial design notes", "role":"User" }
}
```

Search:
```
{"query":"design notes","top_k":5,"mode":"Hybrid","alpha":0.6}
```
Answer:
```
{
  "ok": true,
  "contexts":[ { "message_id":"m1","score":0.93,"content":"Initial design notes", ... } ]
}
```

Similar:
```
{"chat_id":"c1","n":5,"project_scoped":true}
```

Config set (change alpha):
```
{"hybrid_alpha":0.7}
```
Config get (redacted):
```
{
  "ok": true,
  "config": {
     "hybrid_alpha":0.7,
     "openai":{"api_key":"****","model":"text-embedding-3-small", ...}
  }
}
```

Re‑embed all:
```
{}
```

---

## 17. Archival Example

Before:
```
messages indices: 0..9 (10 messages)
store_memory_segment_with_summary(0,7,"Earlier greeting & setup", cx)
```
After:
- Messages 0..7 replaced by placeholder summary message.
- Segment recorded with:
  - `message_count = 8`
  - `message_char_count = sum(chars(messages[0..7]))`
  - `placeholder_char_count = summary.len()`
- Future prompt construction excludes original 0..7 raw texts.

---

## 18. UI Numeric Mention Suppression (Streaming)

Streaming duplication fix (summary):
- On each text chunk, adjacent duplicate mention tokens like `[@61]` are collapsed.
- Pure numeric mention tokens (pattern `[ @ digits ]` without a link) removed from user-visible content.
- Other structured mentions (files, symbols, threads, rules) preserved.

Rationale:
- Reduces clutter from model stutter artifacts.
- Maintains meaningful reference links while hiding opaque numeric markers.

---

## 19. Design Principles Recap

- Deterministic snake_case tool surface.
- Explicit JSON envelopes (`ok` flag).
- Separation of responsibilities (store vs adapter).
- Non‑blocking background tasks.
- Error transparency (no silent suppression).
- Minimal but extendable scoring formulas.

---

## 20. Reimplementation Reference Map

| Feature | Crate / Location | Key Entry |
|---------|------------------|-----------|
| Chat core store | chat_history | `ChatStore::new` |
| Tool adapter | chat_history_tools | `ChatHistoryTools` + trait impl |
| Init options | chat_history_tools | `ChatHistoryInitOptions` |
| Embedding backends | chat_history | `FastEmbedBackend`, `OpenAIEmbeddingBackend`, `AzureOpenAIEmbeddingBackend` |
| Memory segments | agent2 | `ThreadMemorySegment` + `Thread::store_memory_segment*` |
| Session registration fix | agent2 | `NativeAgentConnection::new_thread` calling `register_session` |
| Dedup + mention suppression | agent2 | `Thread::handle_text_event` modifications |
| Global memory backend placeholder | assistant_tools / context_management | `GlobalMemoryBackend::set_backend` |
| Enhanced terminal | assistant_tools | Terminal related tool code (jobs listing) |

---

## 21. Suggested Future Extension Points

- Add citations array to `chat_answer` output.
- Provide partial streaming for long RAG answers.
- Introduce semantic chunking for large chats before pooling.
- Parameterize memory segment summary generation model.
- Add filter predicates to search (role filtering, time window).
- Implement index incremental updates (avoid full rebuild on reembed).

---

End of documentation.