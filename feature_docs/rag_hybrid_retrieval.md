# RAG & Hybrid Retrieval Subsystem

This document specifies the Retrieval-Augmented Generation (RAG) and Hybrid Retrieval features used to enrich assistant responses with prior chat context and project history. It is structured so an engineer or coding model can recreate the subsystem from scratch.

---

## 1. Goals

- Provide relevant historical message contexts for a user’s new query.
- Fuse lexical relevance (BM25 / keyword) with semantic embedding similarity.
- Support configurable weighting (`hybrid_alpha`) between the two modalities.
- Efficiently cap context size to remain within model token limits.
- Enable future expansion: richer citation metadata, streaming answers, ANN indices.

Non-goals (current phase):
- Full document chunking beyond message units.
- Multi-hop reasoning across retrieved contexts.
- Advanced re-ranking (RRF, LTR) — left as extension points.

---

## 2. High-Level Architecture

```
+-----------------------+
|  Query / Question     |
+----------+------------+
           |
           v
   +---------------+         +------------------+
   | Lexical Index | ----->  | Lexical Results  |
   |  (BM25)       |         | (messages + score) 
   +---------------+         +------------------+
           |
           |                         +------------------+
           |                         | Embedding Store  |
           |                         | (vectors)        |
           v                         +---------+--------+
   +---------------+                           |
   | Embed Query   |---------------------------+
   | (same model)  |                           v
   +---------------+                   +------------------+
                                       | Semantic Results |
                                       | (messages+cosine)|
                                       +------------------+
                                               |
                             +-----------------+-----------------+
                             | Hybrid Fusion (weighted scoring) |
                             +-----------------+-----------------+
                                               |
                                               v
                                       +------------------+
                                       | Top-K Deduped    |
                                       +--------+---------+
                                                |
                                   +------------+------------+
                                   |  Context Packing        |
                                   | (token budget trim)     |
                                   +------------+------------+
                                                |
                                                v
                                   +--------------------------+
                                   |  Prompt Assembly         |
                                   | (citations + question)   |
                                   +------------+-------------+
                                                |
                                                v
                                        +--------------+
                                        |  LM Answer   |
                                        +--------------+
                                                |
                                                v
                                        +-----------------+
                                        | Final Answer    |
                                        | (text + contexts|
                                        +-----------------+
```

---

## 3. Core Data Structures (Conceptual)

```
Message {
  message_id: String
  chat_id: String
  role: User | Assistant
  content: String
  created_at: DateTime
  token_estimate: usize
  embedding_vector: Option<Vec<f32>>   // same dimensionality across messages
  keyword_terms: Vec<String>           // tokenized content (preprocessed)
}

HybridCandidate {
  message_id: String
  chat_id: String
  lexical_score: f32        // raw or normalized BM25
  embedding_score: f32      // cosine similarity in [0,1]
  fused_score: f32          // final weighted score
  content: String
  role: User | Assistant
}
```

---

## 4. Retrieval Modes

Enum `RetrievalMode`:
- `Bm25`: Only lexical scoring (fast, robust to OOV tokens but misses semantic paraphrases).
- `Embedding`: Only embedding similarity (semantic; may surface loosely related text).
- `Hybrid`: Fusion of both (default).

Selection logic:
```
match mode {
  Bm25      => rank by lexical_score only
  Embedding => rank by embedding_score only
  Hybrid    => fused_score = alpha * embedding_score + (1 - alpha) * normalized_lexical_score
}
```

---

## 5. Lexical Index (BM25)

Minimal implementation outline:
1. For each message, store term frequencies + doc length.
2. Maintain global:
   - `avg_doc_len`
   - `doc_count`
   - `df(term)` (document frequency per term)
3. Query tokenization identical to message preprocessing (lowercase, punctuation stripped, simple splitting).
4. BM25 score for term `t` in message `m`:
   ```
   idf_t = ln( (doc_count - df_t + 0.5) / (df_t + 0.5) + 1 )
   score_m += idf_t * ( (tf_t * (k + 1)) / (tf_t + k * (1 - b + b * (len_m / avg_doc_len))) )
   ```
   Recommended baseline: k=1.2, b=0.75.

Normalization:
- To unify scale with embedding similarity (0..1), track max lexical score among current candidates:
  ```
  normalized_lexical_score = lexical_score / max_lexical_score
  ```
- If `max_lexical_score == 0`, fallback to 0 for all lexical scores.

---

## 6. Embedding Similarity

- Use the same embedding model for query and messages.
- Query embedding created once per request.
- Cosine similarity:
  ```
  cosine = (q ⋅ m) / (||q|| * ||m||)
  ```
- Ensure vectors are L2-normalized at embedding time to avoid recomputing norm on each query.

Caching:
- Per-message embedding cached via digest (hash(content)).
- Query embedding ephemeral (not persisted).

---

## 7. Fusion (Hybrid Mode)

Formula:
```
fused = alpha * embedding_score + (1 - alpha) * normalized_lexical_score
```
Properties:
- `alpha` ∈ [0,1], defaults around 0.55 balancing semantic preference.
- If one modality unavailable (e.g., no embedding vector yet), degrade gracefully:
  - Missing embedding: `fused = (1 - alpha) * normalized_lexical_score`
  - Missing lexical (rare): `fused = embedding_score`

---

## 8. Ranking & Deduplication

Steps:
1. Collect lexical candidates (up to `L`).
2. Collect semantic candidates (up to `S`).
3. Merge on `message_id`:
   - If message appears in both, combine scores.
   - If only in one, set missing score to 0.
4. Compute fused score (mode dependent).
5. Sort descending by chosen score.
6. Deduplicate on `message_id` (should already be unique post merge).
7. Clip at `top_k` (default from config, e.g. 6 for RAG answer).

---

## 9. Context Packing (Token Budget)

Inputs:
- `top_k` sorted candidates.
- Approximate token cost per message (pre-stored or estimated via heuristic: `tokens ≈ chars / 4` for English).
- `max_context_tokens` (derived from model capacity minus prompt overhead).

Algorithm:
```
budget = max_context_tokens
packed = []
for candidate in candidates:
  cost = candidate.token_estimate
  if cost <= budget:
     packed.push(candidate)
     budget -= cost
  else:
     break
```
Optional tail strategy: if last message overflows by small margin, truncate message content (store truncated flag).

---

## 10. Prompt Assembly

Template (conceptual):
```
System:
You are an assistant that can reference prior conversation snippets.

Context Snippets:
[1] (chat_id=..., message_id=..., role=User)
<content>

[2] (chat_id=..., message_id=..., role=Assistant)
<content>
...

User Question:
<question>

Instructions:
Provide a direct answer. Cite snippets with bracketed numbers [1], [2], etc. when referencing them.
```

Citations embed ordinal indices matching order in `packed`.

---

## 11. Answer Generation

- Single completion call (non-streaming initial phase).
- Temperature low (e.g. 0.2) to encourage grounded output.
- Output parsing: raw answer text; optionally scan for `[n]` patterns to confirm citation usage.

Future expansion:
- Return structured JSON: `{ "answer": "...", "citations": [ { index: 1, message_id: ..., span?: ... } ] }`

---

## 12. Tool / API Contract

For a `chat_answer` style tool:
Input JSON:
```
{
  "question": "How did we handle caching?",
  "project_id": "proj-123?",
  "chat_id": "optional-scope",
  "top_k": 8?,
  "mode": "Hybrid|Bm25|Embedding",
  "alpha": 0.6?
}
```

Output JSON (baseline):
```
{
  "ok": true,
  "answer": "We implemented digest-based caching for embeddings..."
}
```

(Search tool returns `contexts` array for direct inspection; answer tool may return only final text unless contexts are added.)

---

## 13. Failure Modes & Handling

| Failure | Cause | Strategy |
|---------|-------|----------|
| Empty results | No matching messages | Return answer indicating insufficient historical context or fallback to direct answer without citations. |
| Embedding backend unavailable | Misconfiguration / key missing | Log error; degrade to BM25 mode automatically. |
| Over budget context | Large messages | Truncate last message or reduce `top_k`. |
| Missing embeddings for new messages | Embedding task not finished | Treat embedding_score=0 until available. |
| Corrupt vector length | Model change mid-storage | Recompute affected embeddings; quarantine message until fixed. |

Adapter error envelope:
```
{ "ok": false, "error": "<details>" }
```

---

## 14. Performance Considerations

- Precompute & store normalized message vectors (unit norm) to reduce cosine overhead to dot product.
- Maintain inverted index in memory for BM25 (rebuild on startup or on demand).
- For large corpora (>10k messages):
  - Introduce ANN (HNSW or IVF) for embedding search.
  - Implement incremental update queue (avoid full re-embedding).
- Batch embed new messages (e.g., collect up to N pending messages before calling backend).

---

## 15. Embedding Digest Strategy

Digest:
```
digest = SHA256(lowercase(content))
```
Cache:
- Map `digest -> vector`.
- On append, lookup digest first; reuse vector if found.

Benefit:
- Repeated boilerplate / signatures cost zero extra embedding calls.

---

## 16. Normalization & Calibration

Lexical score normalization options:
1. Max scaling (current): `score / max_score`.
2. Min-max scaling (future): `(score - min) / (max - min)`.
3. Percentile scaling (future) for robustness to outliers.

Embedding similarity already in [0,1] if vectors L2-normalized and negative values avoided (some models produce negative components; cosine may be <0; clamp `<0` to 0 for fusion if desired).

Optional clamp:
```
embedding_score = embedding_score.max(0.0)
```

---

## 17. Extensibility Hooks

Potential trait additions:
```
trait ReRanker {
  fn rerank(&self, candidates: &[HybridCandidate]) -> Vec<HybridCandidate>;
}
```

Future modules:
- `CitationExtractor` for mapping text spans to context indices.
- `Segmenter` for splitting long messages into semantic chunks pre-index.

---

## 18. ASCII Sequence Diagram (Answer Flow)

```
User -> ToolAdapter: chat_answer(question, mode=Hybrid)
ToolAdapter -> Store: retrieve_candidates(question, Hybrid)
Store -> LexicalIndex: bm25(query_tokens)
LexicalIndex --> Store: lexical_results
Store -> EmbedBackend: embed([question])
EmbedBackend --> Store: query_vector
Store -> VectorDB/Cache: top_semantic(query_vector)
VectorDB/Cache --> Store: semantic_results
Store: fuse(lexical_results, semantic_results, alpha)
Store: pack(top_k, token_budget)
Store -> LM: completion(prompt_with_contexts)
LM --> Store: answer_text
Store --> ToolAdapter: { ok: true, answer: answer_text }
ToolAdapter --> User: answer
```

---

## 19. Reconstruction Checklist

1. Implement message store with:
   - Append (assign token estimates).
   - Maintain inverted index (term -> postings).
   - Maintain message vectors (optional lazy embedding).
2. Implement embedding backend abstraction + at least a dummy backend for testing.
3. Implement query pipeline:
   - Tokenize user question.
   - Lexical scoring (BM25).
   - Embed question + compute cosine vs message vectors.
   - Normalize lexical scores.
   - Fuse scores with `alpha`.
   - Sort & select top_k.
4. Implement context packing (simple greedy).
5. Build prompt assembly with numbered citations.
6. Provide answer tool endpoint (`chat_answer`).
7. Add configuration object (alpha, top_k default, retrieval mode).
8. Add error handling & JSON envelope responses.
9. Optional tests:
   - Pure BM25 vs Hybrid difference.
   - Embedding-only fallback correctness.
   - Token budget enforcement.

---

## 20. Example Pseudocode (Fusion Segment)

```
fn hybrid_candidates(query, mode, alpha, top_k):
    lex = bm25_search(query.tokens)
    sem = embedding_search(query.embedding)
    max_lex = lex.iter().map(|c| c.score).fold(0.0, f32::max)

    map = HashMap::new()
    for l in lex:
        map.entry(l.id).or_insert(HybridCandidate { lexical_score: l.score, embedding_score: 0.0, ... })
    for s in sem:
        entry = map.entry(s.id).or_insert(HybridCandidate { lexical_score: 0.0, embedding_score: 0.0, ... })
        entry.embedding_score = s.score

    for cand in map.values_mut():
        let norm_lex = if max_lex > 0.0 { cand.lexical_score / max_lex } else { 0.0 }
        cand.fused_score = match mode {
            Bm25 => norm_lex
            Embedding => cand.embedding_score
            Hybrid => alpha * cand.embedding_score + (1.0 - alpha) * norm_lex
        }

    let mut out: Vec<_> = map.into_values().collect()
    out.sort_by(|a,b| b.fused_score.partial_cmp(&a.fused_score).unwrap())
    out.truncate(top_k)
    out
```

---

## 21. Edge Case Handling Summary

| Case | Handling |
|------|----------|
| All lexical scores zero | norm_lex = 0 => fused relies on embedding only |
| Negative cosine (rare) | Optionally clamp to 0 to avoid penalizing fusion |
| Duplicate messages (same content) | Both appear separately; consider collapsing by identical digest if desired |
| Large messages early | Accept until budget exhausted, then stop |
| Sparse embeddings (few vectors computed yet) | Degrade embeddings missing to 0; rely on lexical until vectors populate |

---

## 22. Future Enhancements

- Reciprocal Rank Fusion (RRF) replacing linear alpha weighting.
- Semantic chunking for long messages (split into topically coherent segments).
- Adaptive alpha: data-driven weighting depending on query length or term rarity.
- Cross-project retrieval with project boosting factor.
- Feedback loop: record whether retrieved contexts led to accepted answer; update ranking heuristics.

---

## 23. Minimal Testing Matrix

| Test | Description | Expected |
|------|-------------|----------|
| Hybrid vs BM25 | Query with paraphrase not sharing keywords | Hybrid returns semantic matches; BM25 misses |
| Alpha extremes | alpha=0 and alpha=1 | Matches lexical-only / embedding-only ranking |
| Token budget trim | Budget < total candidate tokens | Truncated list; order preserved |
| Missing embeddings | Newly appended messages unembedded | Fused uses lexical only; still ranks |
| Repeated content digest | Same message appended twice | Second embedding reused (vector identical) |

---

## 24. Security / Safety Considerations

- Avoid prompt injection via raw retrieved content by:
  - Optionally sanitize system-level directives in retrieved messages (future).
  - Keep retrieved user messages verbatim for accuracy; consider classifier for unsafe content.
- Never include user API keys in retrieval contexts.
- Limit maximum contexts to prevent runaway prompt expansion.

---

## 25. Summary

Hybrid retrieval enriches answers by balancing deterministic lexical matches against semantic similarity, governed by a configurable alpha. The subsystem remains modular: indexing, embedding, fusion, packing, prompt assembly, and answering steps can be swapped or extended independently.

Reimplementation should focus first on correctness and clarity (simple data structures, clear scoring), then layer optimizations (ANN, advanced normalization, re-ranking) as scale demands.

---