# Global Memory Backend Feature Documentation

This document specifies the Global Memory Backend feature introduced on the feature branch.
It enables *cross–thread / cross–chat* long‑term semantic memory that agents and tools can query,
independent of the per‑thread `ChatStore` or transient agent2 `Thread` entities.

The intent is that a fresh upstream checkout could use this spec to reconstruct the subsystem.

---

## 1. Purpose & Scope

| Aspect | Included | Excluded (for now) |
|--------|----------|--------------------|
| Cross‑thread recall | ✅ | |
| Persistent semantic facts | ✅ (if backend supports) | Automatic grooming |
| Vector search abstraction | ✅ | ANN index (HNSW / IVF) |
| Pluggable engines | ✅ | Orchestration of multiple engines simultaneously |
| Write throttling | ✅ (design expectation) | Advanced retention policies |
| Secrets management | Out of scope | Encryption key rotation |

The Global Memory Backend sits above specialized storages (chat history, thread memory segments) and
below agent reasoning. It centralizes concept storage (facts, summaries, embeddings) for reuse across sessions.

---

## 2. High‑Level Architecture

```
+-------------------------------------------------------------+
|                        Agent Runtime                        |
|  (agent2 Thread, Chat Tools, Enhanced Terminal, RAG usage)  |
+---------------------------+---------------------------------+
                            | (API Calls)
                            v
+-------------------------------------------------------------+
|               Global Memory Backend Facade                  |
|  - Registration / replacement (set_backend)                 |
|  - Normalized request structs                               |
|  - Error propagation / logging hooks                        |
+---------------------------+---------------------------------+
            (dyn trait object Arc<>)        ^
                                            |
                                            v
+-------------------------------------------------------------+
|                 Concrete Backend Implementations            |
|  * InMemoryBackend (reference impl)                         |
|  * VectorStoreBackend (embeddings + metadata)               |
|  * HybridBackend (lexical + vector)                         |
+-------------------------------------------------------------+
```

**Key invariant:** At any point there is at most ONE active backend instance; swapping replaces it atomically.

---

## 3. Core Trait (Conceptual)

```rust
pub trait MemoryBackend: Send + Sync {
    /// Insert or update a memory entry. Returns assigned ID (stable).
    fn upsert(&self, entry: MemoryEntry) -> Result<MemoryId>;

    /// Fetch by ID.
    fn get(&self, id: &MemoryId) -> Result<Option<MemoryEntry>>;

    /// Delete by ID (soft or hard depending on backend).
    fn delete(&self, id: &MemoryId) -> Result<bool>;

    /// Semantic / lexical / hybrid search.
    fn search(&self, request: MemorySearchRequest) -> Result<Vec<MemorySearchResult>>;

    /// Batch embedding re‑computation (if backend stores vectors).
    fn reembed(&self, ids: &[MemoryId]) -> Result<()>;

    /// Optional maintenance tick (compaction / eviction).
    fn maintenance(&self) -> Result<()>;
}
```

### 3.1 Data Types (Conceptual)

```
MemoryId          = newtype(String or UUID)
TimestampMs       = u128 (epoch milliseconds)

MemoryEntry {
  id: Option<MemoryId>     // None => assign new
  project_id: Option<String>
  namespace: Option<String> // logical partition (e.g. "facts", "tools", "summaries")
  title: Option<String>
  body: String              // canonical text
  tags: Vec<String>
  embedding: Option<Vec<f32>>
  created_at: TimestampMs
  updated_at: TimestampMs
  importance: Option<f32>   // heuristic score (user / system assigned)
}

MemorySearchRequest {
  query: String
  top_k: usize              // limit
  mode: SearchMode          // Lexical | Embedding | Hybrid
  min_score: Option<f32>
  tag_filter: Option<Vec<String>>
  namespace_filter: Option<Vec<String>>
  project_filter: Option<Vec<String>>
}

MemorySearchResult {
  id: MemoryId
  score: f32
  snippet: String
  title: Option<String>
  tags: Vec<String>
  namespace: Option<String>
  project_id: Option<String>
  // optional structured attribution
}
```

### 3.2 Search Modes

| Mode | Description |
|------|-------------|
| Lexical | BM25 / token frequency score only |
| Embedding | Cosine similarity on precomputed vectors |
| Hybrid | Weighted fusion: `final = α * cosine + (1 - α) * normalized_lexical` |

`α` is backend or config controlled (default 0.55).

---

## 4. Registration / Switching

### 4.1 Function (Observed Warning Stub)

```
GlobalMemoryBackend::set_backend(cx: &mut App, backend: Arc<dyn MemoryBackend>)
```

Behavior (spec):
1. Atomically replaces the active backend pointer stored in a global state entity.
2. Triggers *optional* migration: (if previous backend differs and supports export/import).
3. Logs a debug line with backend type + model name (if embedding enabled).
4. Notifies any watchers (e.g., UI panels) to refresh memory-related displays.

Edge cases:
- Repeated set with same backend pointer: no-op.
- Attempt to set while an ongoing maintenance task holds exclusive lock: enqueue and apply after completion (simple approach: just block; advanced approach: queue).

---

## 5. Interaction With Other Features

| Feature | Interaction |
|---------|------------|
| Chat History | Can store distilled summaries (e.g. chat synopsis) as autonomous memory entries. |
| Memory Segments | Archived thread segments may produce a condensed memory entry at the time of archiving. |
| RAG | Hybrid retrieval can include global memory results + chat messages for richer context. |
| Enhanced Terminal | CLI outputs (e.g. `ls -la` results) can be summarized and inserted as memory when marked important. |
| Azure/OpenAI Embeddings | Provide embedding vectors for entries during `upsert` or deferred re-embedding. |

Potential pipeline for a new Assistant turn:
1. Collect top-K global memory matches.
2. Collect relevant chat history messages.
3. Compose unified context block with source annotations (`GM#1`, `CH#2`, etc.).

---

## 6. Lifecycle & Maintenance

### 6.1 Insert Flow

```
User / Tool Action --> upsert(entry)
  -> If embedding needed and embedding service ready:
       compute vector (async, store placeholder, fill later)
  -> Persist metadata immediately
  -> Return MemoryId
```

### 6.2 Re-Embedding Triggers

- Model change (embedding provider switch).
- Explicit `reembed(ids)` call.
- Periodic maintenance (e.g., stale entries without vectors).

### 6.3 Eviction / Compaction (Future)

Policy placeholder:
- Soft delete: mark `importance = -inf` or tag `deleted`.
- Hard delete: physical removal (irreversible).

---

## 7. Error Handling

| Scenario | Strategy |
|----------|----------|
| Embedding backend unavailable | Upsert succeeds with `embedding = None`; search embedding mode excludes such entries. |
| Vector dimension mismatch | Return `Err`; caller may trigger full re-embed. |
| DB connection lost (if persistent backend) | Error surfaced; no silent discard. |
| Query parse failure | Return error early (no partial results). |

All public methods should favor `Result<T>`; never panic on malformed data.

---

## 8. Thread Safety & Concurrency

- Trait requires `Send + Sync`.
- Internal concurrency (e.g. embedding queue) implemented via channels or background executor.
- Search operations may read concurrently; writes (upsert/delete/reembed) serialize per entry or use fine-grained locks.

---

## 9. ASCII Diagrams

### 9.1 Upsert Timeline

```
+----------+       +---------------------+       +------------------+
|  Caller  | ----> | GlobalMemoryFacade  | ----> |  Backend.upsert  |
+----------+       +---------------------+       +------------------+
                           |                            |
                           | log/debug                  | compute embedding (async)
                           |                            v
                           | <----- MemoryId -----------+
                           v
                   return MemoryId
```

### 9.2 Hybrid Search

```
            Query: "refactor memory segments"
                          |
                          v
                 +--------------------+
                 |  Facade.search()   |
                 +--------------------+
                     /           \
                    /             \
          Lexical index        Embedding index
            (BM25)                 (Cosine)
                \                 /
                 \               /
                  +-------------+
                  | Fusion α    |
                  +-------------+
                         |
                         v
                 Ranked MemoryResults
```

---

## 10. Reimplementation Guide (Step-by-Step)

1. Define trait `MemoryBackend` plus data structs (`MemoryEntry`, `MemorySearchRequest`, `MemorySearchResult`).
2. Implement `InMemoryBackend`:
   - Store entries in `HashMap<MemoryId, MemoryEntry>`.
   - Maintain inverted index (token -> Vec<MemoryId>) for lexical scoring.
   - Optional: compute embeddings immediately (call stub embedding provider).
3. Implement `GlobalMemoryBackend` facade:
   - Holds `Arc<dyn MemoryBackend>` in global state.
   - Provides helper `set_backend`.
   - Exposes convenience wrappers for `upsert`, `search`, `delete`.
4. Add hooking points:
   - On thread memory segment creation → call global memory `upsert`.
   - On chat summary refresh → store summary as memory entry (namespace `"chat_summary"`).
5. Provide configuration object:
   - Embedding alpha `hybrid_alpha`.
   - Top-K default.
6. Add fusion function:
   ```
   fn fuse(alpha: f32, cosine: f32, lexical: f32_normalized) -> f32 {
       alpha * cosine + (1.0 - alpha) * lexical
   }
   ```
7. Testing:
   - Upsert two entries; search lexical-only returns expected order.
   - Embed stub: identical content vectors → cosine 1.0.
   - Hybrid ordering changes when alpha adjusted.
   - Switching backend retains API continuity (existing IDs may or may not migrate depending on chosen design).
8. Logging:
   - On `set_backend`: `debug!("Global memory backend set: {}", backend.model_name())`
   - On embedding failure: `warn!` and continue with lexical.

---

## 11. Example Pseudo-Usage

```rust
// Create backend
let backend = Arc::new(InMemoryBackend::new(Default::default()));

// Register
GlobalMemoryBackend::set_backend(cx, backend.clone());

// Insert
let id = backend.upsert(MemoryEntry {
    id: None,
    project_id: Some("proj-a".into()),
    namespace: Some("facts".into()),
    title: Some("Archiving heuristic"),
    body: "Segments older than 30 messages may be archived.",
    tags: vec!["archive".into(), "heuristic".into()],
    embedding: None,
    created_at: now_ms(),
    updated_at: now_ms(),
    importance: Some(0.8),
})?;

// Search
let results = backend.search(MemorySearchRequest {
    query: "archived segments",
    top_k: 5,
    mode: SearchMode::Hybrid,
    min_score: None,
    tag_filter: None,
    namespace_filter: Some(vec!["facts".into()]),
    project_filter: None,
})?;
```

---

## 12. Extensibility

Future additions:
- **Temporal decay:** degrade score based on age unless pinned.
- **Role-aware search:** combine with user / assistant origin weighting.
- **Structured embeddings:** store separate embeddings for title vs body for better recall.
- **Chunking:** splitting large bodies before embedding for better semantic granularity.

---

## 13. Risks & Mitigations

| Risk | Mitigation |
|------|------------|
| Memory bloat (unbounded entries) | Add importance-based pruning + limit namespace counts. |
| Embedding model drift | Maintain `embedding_model` field per entry for conditional re-embed. |
| Inconsistent scoring across backends | Normalize vector magnitudes; standard lexical normalization. |
| Blocking re-embed calls | Run in background tasks; return immediate scheduling status. |

---

## 14. Observed Current State

- `GlobalMemoryBackend::set_backend` is present but unused.
- No invocation sites yet populating global memory.
- This doc serves as blueprint for full integration.

---

## 15. Minimum Viable Implementation (Checklist)

- [ ] Trait definitions & data structures
- [ ] In-memory backend
- [ ] Facade with `set_backend`
- [ ] Lexical index builder (tokenization + posting lists)
- [ ] Simple cosine embedding stub (if real embedding unavailable)
- [ ] Hybrid fusion function
- [ ] Basic tests (upsert, search lexical, search embedding, fusion ordering)
- [ ] Logging hooks for backend switch

---

## 16. ASCII Class Diagram (Simplified)

```
+---------------------+        +---------------------------+
| GlobalMemoryFacade  |<>------| dyn MemoryBackend         |
| - backend: Arc<dyn> |        | upsert()                  |
| + set_backend()     |        | get()                     |
| + upsert()          |        | delete()                  |
| + search()          |        | search()                  |
+---------------------+        | reembed()                 |
                               +---------------------------+
                                          ^
                                          |
                               +---------------------------+
                               | InMemoryBackend           |
                               | HashMap<MemoryId, Entry>  |
                               | LexicalIndex              |
                               | EmbeddingProvider?        |
                               +---------------------------+
```

---

## 17. Logging Conventions

| Event | Level | Message Pattern |
|-------|-------|------------------|
| Backend set | debug | `Global memory backend set: <backend_name>` |
| Upsert success | trace | `memory upsert id=<id> chars=<len>` |
| Search issued | debug | `memory search mode=<mode> query_len=<n>` |
| Re-embed scheduled | info | `memory reembed count=<m>` |
| Embedding failure | warn | `memory embedding failed id=<id> err=<e>` |

---

## 18. Integration Suggestions

Upon creating / archiving:
- Thread memory segment summary → global memory (namespace: `thread_segment`).
- Chat summary update → global memory (namespace: `chat_summary`).
- Tool invocation producing durable insight (e.g. code analysis) → global memory (namespace: `analysis`).

Provide helper wrappers to reduce duplication:
```
fn remember_thread_segment(thread_id: &str, summary: &str, importance: f32)
```

---

## 19. Testing Matrix

| Test | Inputs | Expected |
|------|--------|----------|
| Upsert basic | entry without id | returns new ID, retrievable |
| Search lexical | query tokens present | score > 0, results ordered |
| Search embedding | identical body vs query | cosine ~1.0 top result |
| Hybrid ordering | α=0.9 vs α=0.1 | high cosine dominates vs lexical dominates |
| Backend switch | different backend instance | subsequent search uses new backend |
| Re-embed missing vectors | entries lacking embedding | vectors populated |
| Delete | existing ID | returns true, subsequent get None |

---

## 20. Rebuild Strategy After Model Change

1. Capture old `embedding_model`.
2. Update global config specifying new model.
3. Enumerate entries with `embedding_model == old`.
4. Batch re-embed; update stored model name.
5. Optionally log progress (every N entries).

---

## 21. FAQ

**Q:** Why only one backend at a time?
> Simplifies consistent scoring. Multi-backend aggregation can be added later via a composite adaptor.

**Q:** Why not unify with chat history store?
> Separation preserves clarity of *conversation transcripts* vs *distilled global knowledge*. Chat history may be pruned independently.

**Q:** How to prevent junk accumulation?
> Introduce importance threshold; periodic maintenance drops entries below threshold unless tagged `pinned`.

---

## 22. Future Evolution

- Multi-tenant isolation (separate namespaces per user).
- Graph memory overlay (entities / relations).
- Attention-based pooling of embeddings.
- Real-time streaming insertion with partial vectors.

---

## 23. Summary

The Global Memory Backend provides a pluggable, centralized repository for cross-session semantic recall. Its design emphasizes:
- Clear trait abstraction
- Configurable retrieval modes
- Safe backend swapping
- Alignment with existing embedding infrastructure

This document should enable reconstruction of the feature in absence of code by re-creating traits, data types, initialization logic, and interactions outlined here.

---
End of global_memory_backend.md