# Embedding Backends (Conceptual Overview)

This document describes the embedding backend subsystem used by the chat history and related agent features. It enables pluggable vector generation for semantic search, similarity, and RAG context building.

## 1. Goals

- Provide a uniform trait so multiple providers (local FastEmbed, OpenAI, Azure OpenAI) can be swapped without changing higher-level logic.
- Support batching for throughput.
- Offer deterministic configuration + override layering.
- Allow future expansion (e.g. GPU acceleration, additional providers) without breaking existing code.

## 2. High-Level Architecture

```
+----------------------+        +--------------------+
|  Configuration Load  | -----> |  EmbeddingBackend  |<------------------+
+----------+-----------+        +---------+----------+                   |
           |                               |                              |
           | selects kind                  | embed(&[String])             |
           v                               v                              |
+----------------------+        +--------------------+                    |
|  Backend Factory     | -----> |  Concrete Backend  |                    |
+----------------------+        +--------------------+                    |
           |                               |                              |
           | returns Arc<dyn EmbeddingBackend>                             |
           v                                                              |
+----------------------+                                                 |
|   ChatStore / Tool   | <-----------------------------------------------+
+----------------------+
```

## 3. Trait Contract (Conceptual)

Conceptually, an embedding backend exposes:
- A stable model identifier (used for recording which vectors were produced by which model).
- A batch embedding capability (accepts a list of texts; returns one vector per text).
- Consistent vector dimensionality across all outputs.
- Transparent error reporting (failures propagate; no hidden retries).

Concrete method signatures and return types are intentionally omitted here; only behavioral expectations are described.

## 4. Supported Backends

| Backend                | Identifier (enum)                | Typical Model                        | Notes |
|------------------------|----------------------------------|--------------------------------------|-------|
| FastEmbed Local        | `FastEmbedLocal { model_path }`  | `bge-base-en-v1.5` / variant         | Pure Rust / local; CPU inference. |
| OpenAI Embeddings API  | `OpenAI { .. }`                  | `text-embedding-3-small`             | Requires API key & optional custom URL. |
| Azure OpenAI Embedding | `AzureOpenAI { .. }`             | Deployment name or overridden model  | Requires endpoint, deployment, api key, version. |

### 4.1 FastEmbed Local

- Instantiates a small transformer model locally.
- Loads weights once (lazy initialization).
- Batches entire request slice; latency dominated by sequence length × dimension.

### 4.2 OpenAI

- Validates presence of API key before first call.
- Model selection: explicit in config or default fallback.
- Handles rate limit errors by surfacing them as `Err(anyhow!(...))` for caller-level decisions.

### 4.3 Azure OpenAI

- Requires: `api_key`, `endpoint`, `deployment`.
- `embedding_model` optional; defaults to `deployment` if unspecified.
- API version has a default (e.g., `2024-02-15-preview`) to simplify initial setup.
- Designed for parity with OpenAI backend but allows Azure-specific overrides.

## 5. Configuration Layering

Order of precedence:
1. Settings file (`.zed/settings.json`).
2. Initialization options (programmatic overrides).
3. Runtime tool update (`chat_config_set` where relevant).

Effective config pseudocode:
```
let config = load_settings();
apply_init_overrides(&mut config, init_options);
apply_runtime_mutations(&mut config, tool_updates);
```

Changing `embedding_model` or switching backend kind triggers a re-embedding scheduling path (tool `chat_reembed` or automatic future enhancement).

## 6. Backend Factory Decision Flow

```
+---------------------------+
| Choose EmbeddingBackendKind
+-----------+---------------+
            |
            | match kind
            v
    +---------------------+
    | FastEmbedLocal?     |-- yes --> instantiate FastEmbedBackend(model_name)
    +---------------------+
            |
            no
            v
    +---------------------+
    | OpenAI?             |-- yes --> validate api_key; create OpenAIEmbeddingBackend(model)
    +---------------------+
            |
            no
            v
    +---------------------+
    | AzureOpenAI?        |-- yes --> validate api_key, endpoint, deployment; create AzureOpenAIEmbeddingBackend
    +---------------------+
```

Validation errors abort initialization (caller sees a failure result).

## 7. Batching Strategy

- Current design sends the entire slice of inputs in one call (unless provider restricts batch size).
- Future optimization: chunking large slices (`N > MAX_BATCH`) into smaller requests with concatenated results.
- Digest caching: identical content strings can skip API calls (if implemented by backend wrapper) by hashing text (e.g., SHA256) and checking a local map.

## 8. Error Handling Principles

- No `unwrap()`; use `?` propagation.
- Provider-specific HTTP/network errors are wrapped in `anyhow::Error` with context.
- Input validation (empty slice) returns an empty vector quickly (O(1))—or an error if the provider disallows empties.

## 9. Vector Semantics

- All vectors for a given backend share consistent dimensionality.
- Dimension typically implied by model (e.g., 768, 1024).
- Consumers (search, pooling) assume consistent dimension; re-embedding ensures old vectors are replaced when switching models.

## 10. Pooling

- Chat-level vector = arithmetic mean of message vectors.
- If no message vectors ready yet, `chat_vector` remains `None`.
- In partial re-embed states, only available vectors are included (sum/ count).

Formula:
```
chat_vector[d] = (Σ message_vector_i[d]) / count_vectors
```

## 11. Hybrid Search Fusion

While not implemented inside backend itself, the backend supplies raw embedding vectors used in the fusion:
```
score = alpha * cosine_similarity + (1 - alpha) * keyword_score_norm
```
Backend choice affects cosine distribution (some models yield narrower or broader similarity ranges; alpha can be tuned accordingly).

## 12. Re-Embedding Workflow

1. User changes backend or embedding model.
2. Schedule re-embedding:
   - Enumerate all messages (optionally scoped by chat).
   - Recompute vectors.
   - Recompute chat pooled vectors.
3. Update metadata (timestamp, embedding model recorded).
4. Invalidate any search cache (if present).

## 13. ASCII Sequence Diagram (Append Path With Embedding)

```
User Tool Call: chat_append
        |
        v
+------------------+        +-----------------------+
|  ChatHistoryTools | ----> |   ChatStore           |
+---------+---------+        +-----------+-----------+
          |                           |
          | persist message           | enqueue embedding task
          |                           v
          |                     +-----------+
          |                     | Embedding |
          |                     | Backend   |
          |                     +-----+-----+
          |                           |
          |                vectors returned
          |                           |
          v                           v
+------------------+        +-----------------------+
|  ChatStore       | <----- |  Update message & chat|
+------------------+        +-----------------------+
```

## 14. Performance Considerations

| Area           | Current | Future Optimization |
|----------------|---------|---------------------|
| Local Backend  | CPU only | Add SIMD / GPU if needed |
| Remote Backends| Single batch / request | Parallel chunked calls |
| Caching        | Digest optional | Persistent vector cache |
| Re-embedding   | Full recompute | Incremental diff-only updates |

## 15. Security / Secrets Handling

- API keys loaded into config; redacted on `config_get` output (`"****"`).
- Keys stored only in memory + settings file (not embedded in chat message objects).
- Rotation requires reinitialization or runtime config set call (followed by re-embedding as needed).

## 16. Failure Modes

| Failure | Cause | Mitigation |
|---------|-------|------------|
| Missing API Key | Misconfiguration | Validation error with clear message |
| Network Timeout | Provider latency | Retry/backoff (future) |
| Dimension Mismatch | Wrong model swap mid-cycle | Forced re-embed from scratch |
| Empty Batch Crash | Provider rejects | Return early with empty result or explicit error |
| Rate Limit | High request frequency | Surface error, allow caller to slow down |

## 17. Extension Points

- Add `Cohere`, `Voyage`, or other provider enum variants.
- Introduce streaming embeddings (partial results) for very large texts.
- Configure max batch size and dynamic splitting.
- Weighted pooling (attention) instead of mean pooling.

## 18. Implementation Checklist (Reconstruction)

1. Define `EmbeddingBackendKind` enum with variants + associated data.
2. Implement trait `EmbeddingBackend`.
3. Provide concrete backends:
   - FastEmbed: loads model name; performs inference locally.
   - OpenAI: constructs HTTP requests; handles errors.
   - Azure: similar, with endpoint + deployment specifics.
4. Factory:
   - Match kind -> validate required fields -> instantiate backend.
5. ChatStore integration:
   - Accept `Arc<dyn EmbeddingBackend>`.
   - On append: schedule embedding job (spawn background task).
6. Pooled chat vector update after message embedding results.
7. Re-embedding function:
   - Iterate messages -> embed -> update vectors -> recompute pooled vectors.
8. Config mutation triggers scheduling of re-embedding (explicit or automatic).
9. Tests:
   - Ensure model name propagation.
   - Ensure failure scenarios produce errors (missing keys).
   - Ensure vector length uniformity across batches.

## 19. Example JSON Config Snippet (Settings File)

```
{
  "chat_history": {
    "embedding_model": "bge-base-en-v1.5",
    "hybrid_alpha": 0.55,
    "similar_chats_k": 10,
    "default_retrieval_mode": "Hybrid",
    "openai": {
      "api_key": "sk-...REDACTED...",
      "model": "text-embedding-3-small"
    },
    "azure_openai": {
      "api_key": "",
      "endpoint": "",
      "deployment": "",
      "api_version": "2024-02-15-preview"
    }
  }
}
```

## 20. Conceptual Usage Example

Conceptual flow:
1. Select backend kind from configuration (e.g. local vs remote).
2. Construct backend instance (validating required credentials if remote).
3. Provide a batch of input texts.
4. Receive a vector per input, all sharing the same dimensionality.
5. Store vectors and (optionally) recompute any pooled representations.

Exact code, types, and error handling details are intentionally excluded.

## 21. Future Considerations

- Add normalized confidence scores to each vector for downstream weighting.
- Integrate tokenizer to compute exact token counts (vs heuristic estimates).
- Pluggable dimensionality reduction (PCA) for faster similarity scans when large corpus emerges.

---

End of embedding backends documentation.