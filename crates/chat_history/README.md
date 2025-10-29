# chat_history

The `chat_history` crate provides persistent storage, similarity search, hybrid retrieval, and lightweight RAG (retrieval‑augmented generation) over previous assistant chats. It is designed to let an LLM agent reuse prior conversation context efficiently and safely.

## Goals

* Auto-save: Every new message appended to a chat is persisted asynchronously.
* Structured metadata: Each chat maintains summary, title, project, message count, character count, token estimate, timestamps, tags, archived/pinned flags, and a pooled embedding vector.
* Message-level embeddings: Each message may have its own embedding to enable fine-grained semantic retrieval and accurate citations.
* Similar chats: Top-N (default 10) similar chats based on whole-chat embedding pooled from message vectors.
* Hybrid search: BM25 / keyword + embedding similarity fusion for messages, scoped optionally to a project or a single chat.
* RAG answer: A question answering function that retrieves relevant past messages and produces a capped context set and answer with citations.
* Pluggable embedding backends: FastEmbed (default: `bge-base-en-v1.5`), OpenAI, Azure OpenAI—switchable via configuration.
* Async operations: Embeddings, summaries, and reindexing run in background tasks to avoid blocking the UI foreground thread.
* Extensibility: Clear trait boundaries around embedding providers and storage layer.

## Non-Goals (Initial Phase)

* Encryption-at-rest (can be layered later).
* Streaming partial RAG answers (one-shot final answer for phase 1).
* Incremental chat embedding updates (full recompute for correctness).
* Approximate nearest neighbor index (HNSW) at initial scale (~1000 chats total); can be added when needed.

## Data Model (Conceptual)

ChatMetadata:
- `chat_id`
- `project_id`
- `title`
- `summary`
- `created_at` / `updated_at`
- `total_messages`
- `total_characters`
- `token_estimate`
- `embedding_model`
- `archived` / `pinned`
- `tags`
- `chat_vector` (Option<Vec<f32>>)

ChatMessage:
- `message_id`
- `chat_id`
- `role`
- `content`
- `created_at`
- `token_estimate`
- `tags` (optional future expansion)
- `embedding_digest` (hash for reuse)
- (Vector stored via embedding table join or cache)

## Configuration

`ChatHistoryConfig` (defaults):
- `embedding_backend`: FastEmbed local
- `embedding_model`: `bge-base-en-v1.5`
- `hybrid_alpha`: 0.55 (weight for cosine vs keyword score)
- `similar_chats_k`: 10
- `summary_refresh_chars`: 4000
- `summary_delta_chars`: 1500
- `rag_top_k`: 6
- `auto_tag`: true
- `default_retrieval_mode`: `Hybrid` (options: `Bm25`, `Embedding`, `Hybrid`; hybrid combines lexical BM25 + embedding cosine, weighted by `hybrid_alpha`)

Changing configuration triggers:
- Potential re-embedding (if model changed).
- Updated fusion weighting for hybrid search.
- Adjusted thresholds for summary refresh.

## Embedding Backends

`EmbeddingBackend` trait:
- `model_name(&self) -> &str`
- `embed(&self, &[String]) -> Result<Vec<Vec<f32>>>`

Planned implementations:
- FastEmbed (`bge-base-en-v2.5` by default; path override allowed).
- OpenAI (requires API key & model name).
- Azure OpenAI (API key + endpoint + deployment).

## Search & Similarity

Hybrid scoring formula (initial):
```
score = alpha * cosine_similarity + (1 - alpha) * normalized_keyword_score
```
Normalization strategies will evolve (e.g. BM25 max tracking, percentile scaling).

Similar chats:
1. Compute / retrieve cached `chat_vector`.
2. Cosine similarity against other chats (project-scoped by default).
3. Return top-N with scores; if vectors missing fall back to lexical approximation.

## RAG Answer Flow

1. Retrieve candidate messages (hybrid or embedding-only).
2. Deduplicate and enforce token/context budget.
3. Construct prompt with numbered citations.
4. Call upstream completion provider to generate answer.
5. Return `RagAnswer { answer, contexts }`.

## Auto-Save & Summarization

On each appended message:
- Persist message.
- Update chat metadata counts.
- Schedule embedding for the message (if backend ready).
- If character threshold or delta exceeded, schedule summary refresh.

## Tool / API Surface (Planned Snake Case)

- `chat_create`
- `chat_append_message`
- `chat_get`
- `chat_list`
- `chat_similar`
- `chat_search_messages`
- `chat_answer_question`
- `chat_update_metadata`
- `chat_reembed`
- `chat_config_get`
- `chat_config_set`

Returned JSON will evolve; initial iteration includes core fields and may add an `error` string for graceful failure cases.

## Error Handling Philosophy

* Prefer `Result<T>` propagation over panics.
* Fail fast on structural inconsistencies (e.g., embedding length mismatch).
* Use explicit logging hooks for background task failures (not silent).

## Performance Considerations

* Embedding batching groups messages to reduce overhead.
* Digest-based caching prevents recomputing identical text embeddings.
* Mean pooling chosen for simplicity; can upgrade to attention-weighted pooling later.

## Roadmap

Phase 1:
- Crate skeleton (DONE).
- Persistence integration (DB migrations / SeaORM models).
- FastEmbed backend.
- Basic similarity + hybrid search + RAG stub returning placeholder answers.

Phase 2:
- Complete RAG integration with actual answering.
- Summary auto-generation.
- Tag auto-suggestion (keyphrase extraction).
- Config UI hook.

Phase 3:
- ANN indexing (HNSW) if corpus size warrants.
- Advanced score fusion (RRF / reciprocal rank).
- Optional streaming result APIs.

## Testing Strategy

* Unit tests for pooling, fusion logic, digest hashing.
* Integration tests: create chat, append messages, search, similar retrieval, RAG answer path.
* Property tests for score fusion boundaries.

## Contributing

Follow workspace Rust guidelines:
- Propagate errors (`?`) rather than panicking.
- Comment only on non-obvious rationale (avoid restating code).
- Avoid `unwrap()` / unchecked indexing.
- No silent error discards.

## License

Dual-licensed under MIT or Apache-2.0 (aligned with broader workspace licensing).

---
Placeholder documentation; will expand as implementation lands.