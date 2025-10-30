# Chat History Core (`chat_history`)

This document describes the design of the Chat History core subsystem. It is intended for an engineer or coding LLM to recreate the crate on a fresh upstream checkout. It focuses on the storage model, embedding workflows, hybrid retrieval, RAG (retrieval‑augmented generation) interface, summarization triggers, configuration layering, and error handling philosophy.

---

## 1. High‑Level Purpose

The Chat History core provides:
- Persistent storage of chats and messages
- Incremental metadata maintenance (counts, estimates, tags)
- Per‑message embeddings + pooled chat vector
- Hybrid lexical / semantic retrieval
- Similar chat discovery
- RAG answer facility (retrieve relevant past content + synthesize answer)
- Automatic summary refresh based on size and delta thresholds
- Configurable embedding backends (FastEmbed local, OpenAI, Azure OpenAI)

Non‑goals for the current iteration:
- Encryption at rest
- Streaming partial RAG answer increments
- ANN (approximate nearest neighbor) index (linear scan until corpus scale demands otherwise)

---

## 2. Architecture Overview

```
+----------------------------+
| ChatHistoryConfig          |
|  (loaded + overridden)     |
+-------------+--------------+
              |
              v
+----------------------------+           +-------------------------+
| EmbeddingBackend (trait)   |<-- dyn -->| FastEmbed / OpenAI /    |
|  model_name()              |           | AzureOpenAI backends    |
|  embed(&[String]) -> Vec[] |           +-----------+-------------+
+-------------+--------------+                       |
              |                                       |
              v                                       |
+----------------------------+                       |
| ChatStore                  |-----------------------+
| - chats: Map<ChatId,...>   |
| - messages: Map<ChatId,Vec>|
| - backend: Arc<dyn ...>    |
| - config: ChatHistoryConfig|
| - optional db handle       |
+-------------+--------------+
              |
      tool_* API surface
              |
              v
+----------------------------+
| External tools / adapters |
| (e.g. JSON snake_case)    |
+----------------------------+
```

---

## 3. Data Model

### 3.1 Identifiers

- `ChatId(String)` – Unique ID per chat (UUID or short random string).
- `MessageId(String)` – Unique ID per message (UUID).

### 3.2 Chat Metadata

```
struct ChatMetadata {
  chat_id: ChatId
  project_id: Option<String>
  title: Option<String>
  summary: Option<String>
  created_at: DateTime<Utc>
  updated_at: DateTime<Utc>
  total_messages: usize
  total_characters: usize
  token_estimate: usize
  embedding_model: String     // active embedding model
  archived: bool
  pinned: bool
  tags: Vec<String>
  chat_vector: Option<Vec<f32>> // pooled from message vectors
}
```

### 3.3 Chat Message

```
enum MessageRole { User, Assistant }

struct ChatMessage {
  message_id: MessageId
  chat_id: ChatId
  role: MessageRole
  content: String
  created_at: DateTime<Utc>
  token_estimate: usize
  embedding_digest: Option<String> // hash(content) for caching
  vector: Option<Vec<f32>>
}
```

### 3.4 Embedding Digest

Digest (e.g. SHA256 of normalized UTF‑8 content) prevents duplicate vector computation:  
- On append: compute digest  
- If digest previously seen → reuse existing vector  
- Else schedule embedding

---

## 4. Configuration Layering

Order of application:

```
Settings File (.zed/settings.json)
        ↓
Explicit Initialization Overrides (ChatHistoryInitOptions)
        ↓
Runtime Mutation (chat_config_set)
        ↓
Effective ChatHistoryConfig in ChatStore
```

### 4.1 ChatHistoryConfig Fields

```
embedding_model: String
hybrid_alpha: f32                // fusion weight (0..1); higher => more semantic
similar_chats_k: usize
summary_refresh_chars: usize     // absolute threshold
summary_delta_chars: usize       // delta since last summary
rag_top_k: usize
auto_tag: bool
default_retrieval_mode: RetrievalMode (Bm25 | Embedding | Hybrid)
openai { api_key?, api_url?, model? }
azure_openai { api_key?, endpoint?, api_version?, deployment?, embedding_model? }
```

---

## 5. Embedding Backends

### 5.1 Trait

```
trait EmbeddingBackend {
  fn model_name(&self) -> &str;
  fn embed(&self, batch: &[String]) -> Result<Vec<Vec<f32>>>;
}
```

### 5.2 Implementations

| Backend          | Notes |
|------------------|-------|
| FastEmbedBackend | Local CPU (e.g. BGE models). Accepts model name or path. |
| OpenAIEmbeddingBackend | Requires API key + model; uses OpenAI REST. |
| AzureOpenAIEmbeddingBackend | Requires endpoint, deployment, api_version, key. Embedding model may default to deployment. |

### 5.3 Batch Strategy

- Collect pending messages lacking vectors into batches of size N (tunable, e.g. 32–128).
- Single backend `embed` call per batch.
- Map outputs back to messages using original order.

---

## 6. Pooled Chat Embedding

Mean pooling:

```
let vectors = all message vectors present
let d = vectors[0].len()
let mut pooled = vec![0.0; d]
for v in vectors {
  for i in 0..d {
    pooled[i] += v[i]
  }
}
for i in 0..d {
  pooled[i] /= vectors.len() as f32
}
chat_vector = Some(pooled)
```

Skipped if no message vectors available.

---

## 7. Hybrid Retrieval

### 7.1 Retrieval Modes

```
enum RetrievalMode {
  Bm25,
  Embedding,
  Hybrid
}
```

### 7.2 BM25 Lexical Index

- Index built over message content per project / global.
- On append: incrementally update index (or mark for periodic rebuild).
- On rebuild flag: full reconstruction from persisted messages.

### 7.3 Fusion Formula

```
final_score = alpha * cosine_similarity + (1.0 - alpha) * normalized_keyword_score
```

Normalization options:
- Divide keyword score by max observed for candidate set.
- Or apply percentile scaling / min‑max if improvements required.

### 7.4 Candidate Selection

1. If mode == Bm25 → top lexical candidates.
2. If mode == Embedding → embed query and rank by cosine.
3. If mode == Hybrid:
   - Gather lexical top M
   - Gather embedding top M
   - Union set; compute both scores for each (missing lexical or embedding treated as 0)
   - Apply fusion; rank descending

### 7.5 Deduplication

- For identical message IDs across lexical and embedding sets, keep one with fused score.

---

## 8. Similar Chats

Process:

1. Ensure each chat has `chat_vector`.  
   - If missing → skip chat or attempt a background generation.
2. Restrict to same project if requested.
3. Compute cosine similarity against target chat’s vector.
4. Sort by similarity descending; return top K (`similar_chats_k` unless overridden).
5. Provide score with metadata.

Cosine:

```
cosine(a,b) = (Σ a_i b_i) / (sqrt(Σ a_i^2) * sqrt(Σ b_i^2))
```

---

## 9. RAG Answer Pipeline

```
+------------------+
| question input   |
+---------+--------+
          |
          v
   Retrieval (mode)
          |
    top_k contexts
          |
   Deduplicate / trim (token budget)
          |
   Build prompt (citations)
          |
   Call model (LLM)
          |
   Answer string
```

### 9.1 Steps

1. Interpret retrieval mode (default: `default_retrieval_mode`).
2. Query store:
   - Hybrid embedding+lexical or single modality
3. Collect contexts up to `rag_top_k`.
4. Estimate tokens; trim oldest or lowest scoring contexts to satisfy token budget.
5. Render prompt sections:
   ```
   [Context #1]
   <message content snippet>
   ---
   [Context #2]
   ...
   Question: <user question>
   ```
6. Invoke upstream completion model (outside of this crate’s responsibility).
7. Return `{ answer: "...", contexts?: [...] }` (contexts optional based on adapter design).

### 9.2 Token Budget

Token estimation can use heuristic: `chars / 4` or a dedicated tokenizer if available. Enforced before prompt construction.

---

## 10. Summarization Logic

### 10.1 Trigger Conditions

Let:
- `total_characters` = current aggregate message chars
- `last_summary_at_chars` = chars at last summary
- `summary_refresh_chars` (absolute threshold)
- `summary_delta_chars` (minimum delta)

Trigger summary refresh if both:
```
total_characters >= summary_refresh_chars
(total_characters - last_summary_at_chars) >= summary_delta_chars
```

### 10.2 Execution

- Spawn background task to generate summary
- Update `ChatMetadata.summary` and `last_summary_at_chars`
- Adjust `updated_at`

### 10.3 Summary Strategy (Initial)

- Simple extractive + compressive approach:
  - Collect earliest messages + representative assistant responses until limit.
  - Apply heuristic compression (truncate & append ellipsis).
- Future improvement: dedicated summarization model; semantic clustering.

---

## 11. Append Flow

### 11.1 Sequence Diagram

```
User/Agent
  |
  | chat_append(content, role, maybe chat_id)
  v
ChatStore
  |-- if chat_id missing: create ChatMetadata (init counts)
  |-- create ChatMessage (assign id, timestamps)
  |-- update totals (messages++, characters += content.len())
  |-- estimate tokens (heuristic)
  |-- compute digest; schedule embedding if new
  |-- maybe trigger summary refresh (see section 10)
  |-- persist (in-memory + DB if available)
  v
Return (metadata, message)
```

### 11.2 Embedding Scheduling

- Store message with `vector = None`
- Background worker batch picks up new messages (accumulate until batch size or timeout)
- After embedding: set message.vector, recompute chat_vector

---

## 12. Search Flow

```
Input query
     |
  Determine mode (Hybrid default)
     |
  Lexical candidates (BM25)
  Embedding candidates (if mode != Bm25)
     |
  Union
     |
  Score fusion (if Hybrid)
     |
  Sort + top_k
     |
  Return contexts
```

Edge cases:
- Empty query → return empty contexts or error
- No messages → return empty array (ok=true)

---

## 13. Re‑Embedding

### 13.1 Triggers

- Manual `reembed` (all chats or specific chat)
- Embedding model change in config

### 13.2 Algorithm

For target scope:
1. Collect messages (optionally filter by missing vector or forced)
2. Clear existing vectors if forced
3. Batch embed
4. Update each message.vector
5. Recompute chat_vector
6. (Optionally) update lexical index if text normalization changed

### 13.3 Idempotency

Repeated calls reapply vectors with the same model; no functional difference aside scaling durations.

---

## 14. Error Handling Philosophy

| Scenario | Behavior |
|----------|----------|
| Parse JSON failure | Return structured error string (no panic) |
| Missing API key for remote backend | Initialization error (fail early) |
| Embedding backend call fails | Mark batch failed; log; set message.vector None; retrieval may fallback to lexical only |
| Unknown retrieval mode | Reject request with explicit error |
| Reembed without chats | Return success; no-op |

Use of `Result<T>` throughout; avoid silent drops. Background tasks log errors.

---

## 15. Performance Considerations

- **Batching**: Minimizes embedding overhead.
- **Digest caching**: Avoids duplicate compute for identical content.
- **Pooling**: O(n*d) simple mean; adequate until d or n large.
- **Linear similarity**: Acceptable for small corpora; HNSW/IVF planned when chat count > threshold (e.g. 10k).
- **Index rebuild**: Optional flag `rebuild_index` at async initialization; avoids unnecessary recompute for small modifications.

---

## 16. Security / Secrets

- Config GET redacts sensitive fields (`api_key`) with `"****"`.
- Internal memory keeps plaintext keys (consider secret manager in production).
- No logging of full API keys; only length or masked form if needed.

---

## 17. Auto Tagging (Basic)

If `auto_tag` enabled:
- Extract top keyphrases via naive heuristic (split by whitespace, filter stopwords, frequency > threshold).
- Insert into `ChatMetadata.tags` (deduplicated).
- Tag removal manual via metadata update call.

Future enhancement: use embedding similarity clustering.

---

## 18. Store API (Conceptual)

```
ChatStore {
  fn append(&mut self, req: AppendRequest) -> Result<(ChatMetadata, ChatMessage)>
  fn search(&self, req: SearchRequest) -> Result<Vec<SearchContext>>
  fn similar_chats(&self, chat_id: &ChatId, k: usize, project_scoped: bool) -> Result<Vec<(ChatMetadata,f32)>>
  fn answer(&self, question: &str, scope: AnswerScope) -> Result<RagAnswer>
  fn reembed(&mut self, target: Option<&ChatId>) -> Result<()>
  fn list(&self, project_id: Option<&str>, limit: usize, offset: usize) -> Result<Vec<ChatMetadata>>
  fn get(&self, chat_id: &ChatId) -> Result<(ChatMetadata, Vec<ChatMessage>)>
  fn update_metadata(&mut self, chat_id: &ChatId, patch: MetadataPatch) -> Result<ChatMetadata>
  fn config(&self) -> &ChatHistoryConfig
  fn set_config(&mut self, new_cfg: ChatHistoryConfig)            // triggers reembed if model changed
}
```

Message / metadata updates must keep invariants:
- `total_messages == messages.len()`
- `total_characters == sum(message.content.len())` (unless archived scheme added later)
- `embedding_model` remains consistent post model change

---

## 19. Rebuild Lexical Index (Async)

```
fn rebuild_message_index(&mut self) {
  // clear existing index
  // iterate all messages
  // tokenize -> add to index structures
  // finalize (compute IDF, normalization data)
}
```

Called during async initialization when `rebuild_index` true and DB connection provided.

---

## 20. Sequence Examples

### 20.1 Append Then Search

```
append("Refactor pipeline", role=User)
append("We added hybrid retrieval", role=Assistant)
search("hybrid retrieval")
 -> lexical hits: both messages (BM25)
 -> embedding hits: similar vectors (cosine)
 -> fusion yields same ordering; returns contexts with scores
```

### 20.2 Update Config (Change alpha)

```
config.hybrid_alpha = 0.7
search => fusion shifts weight to embeddings (semantic synonyms rank higher)
```

### 20.3 Reembed After Model Change

```
set_config(embedding_model="bge-large-en-v1.5")
reembed(None) // all
 -> schedule batches
 -> vectors replaced
 -> chat_vector recomputed
 -> subsequent similar_chats produce different similarity distribution
```

---

## 21. Summary Refresh Example

Initial:
- total_characters = 3800
- summary_refresh_chars = 4000 (not yet triggered)

After appending big message (+500 chars):
- total_characters = 4300
- delta since last summary (assume 0) = 4300 >= summary_delta_chars (e.g. 1500? yes)
→ schedule summary recompute (background)  
→ update summary and last_summary_at_chars = 4300

---

## 22. Extensibility Points

| Point | Description |
|-------|-------------|
| EmbeddingBackend | Add new provider (e.g. local GPU, multi‑vector). |
| Hybrid Scoring | Replace simple linear fusion with Reciprocal Rank Fusion or ML learned weights. |
| Summarization | Introduce semantic chunking + hierarchical summarization. |
| ANN Index | Swap linear scan for HNSW index implementing insert/remove. |
| Auto Tag | Improve with embedding clustering & multiword phrase extraction. |
| Privacy | Add pluggable encryption layer or differential privacy noise. |

---

## 23. Reconstruction Checklist

1. Define IDs, data structs.
2. Implement `ChatHistoryConfig` loader + override layering.
3. Implement `EmbeddingBackend` trait + FastEmbed backend first.
4. Create `ChatStore` with in‑memory maps; optionally stub DB layer trait.
5. Implement append:
   - message creation
   - metadata update
   - digest creation
   - embedding scheduling
   - summary refresh trigger
6. Implement search (BM25 index + embedding retrieval + fusion).
7. Implement similar chats (cosine over `chat_vector`).
8. Implement RAG answer (context retrieval + prompt builder + call external model stub).
9. Implement reembed (scoped or global).
10. Implement config mutation detection (trigger reembed if model changed).
11. Add error propagation (no unwrap).
12. Add tests: append/search/similar/reembed/config override precedence.

---

## 24. ASCII Summary Diagram

```
[Append Request] --> [ChatStore.append]
      |                      |
      |                      +--> [Update Metadata]
      |                      +--> [Digest & Embedding Queue] --> [EmbeddingBackend]
      |                      +--> [Maybe Summary Task]
      |
   Response (ChatMetadata, ChatMessage)

[Search Request] --> [Select Mode] --> [BM25 Index] + [Embedding Similarity]
                                        |                |
                                        +----- Fusion ----+
                                                |
                                            Top-K Contexts

[RAG Answer] --> [Retrieve Contexts] --> [Prompt Build] --> [LLM Call] --> [Answer]
```

---

## 25. Error Examples (JSON Envelope Style)

Not part of core store itself but used by adapter:

```
{ "ok": false, "error": "parse error: expected value" }
{ "ok": false, "error": "openai.api_key not set" }
{ "ok": false, "error": "chat not found: c123" }
{ "ok": false, "error": "embedding batch failed: timeout" }
```

---

## 26. Design Principles Recap

- Deterministic behavior under concurrency (mutexed store access).
- Separation of concerns (embedding backend, lexical index, store).
- Transparent error propagation.
- Extensible configuration without breaking existing operations.
- Minimal runtime footprint until scale demands optimization.

---

End of `chat_history_core.md`.