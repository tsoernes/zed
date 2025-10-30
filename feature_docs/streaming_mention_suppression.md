# Streaming Mention Suppression & Deduplication

This document specifies the feature that cleans up streaming assistant message chunks by:
1. Deduplicating immediately repeated mention tokens at chunk boundaries.
2. Suppressing (removing) numeric-only mention markers of the form `[@<digits>]` that lack semantic payload.
3. Preserving meaningful structured mentions (files, symbols, threads, rules, selections, fetch links).

The goal is to enable a fresh upstream implementation to reproduce behavior closely.

---

## 1. Motivation

LLM streaming output sometimes “stutters”:
- Re‑emitting the same short token (e.g. `[@61]`) across consecutive chunks.
- Producing opaque numeric-only markers the UI doesn’t need to show.

Without suppression, users see duplicated or cryptic artifacts that dilute clarity.

---

## 2. Design Goals

| Goal | Description |
|------|-------------|
| Non‑intrusive | Do not alter legitimate structured mentions. |
| Low overhead | O(n) per chunk; simple string scanning (no heavy regex). |
| Deterministic | Identical input stream yields identical sanitized output. |
| Boundary-aware | Only collapse duplicates that straddle the old/new chunk boundary. |
| Safe suppression | Only remove pure numeric mention tokens; leave other `[ @... ]` forms intact. |
| UI consistency | Event stream receives sanitized chunk so UI, logs, persisted message all match. |

---

## 3. Scope

Affected only during assistant streaming text events. Tool use, thinking sections, and user messages are not modified here.

---

## 4. Terminology

- Existing Text: Accumulated assistant text for the current pending message before new chunk arrives.
- New Chunk: Fresh incoming text fragment from model stream.
- Mention Token:
  - Plain form: `[@X]`
  - Link form: `[@X](payload...)`
  - Numeric-only: `[@123]` (without following parentheses)
  - Structured: `[@file.txt](@file:/path/file.txt)` etc.

---

## 5. High-Level Flow

```
          +--------------------+
Chunk --> | handle_text_event  | --+
          +--------------------+   |
                 |                 |
                 v                 |
     [Deduplicate boundary]        |
                 |                 |
                 v                 |
     [Suppress numeric-only]       |
                 |                 |
                 v                 |
          [Send sanitized] --------+
                 |
          [Append to pending message]
```

---

## 6. Boundary Deduplication Algorithm

Pseudocode sketch:

```
existing = current_text
chunk = incoming_chunk

if existing contains "[@" then
  start_idx = last occurrence of "[@" in existing
  tail = existing[start_idx..]        // candidate trailing mention
  if tail ends with ']' or ')'
     if chunk starts with tail
        // Immediate duplicate mention token
        chunk = chunk[tail.len()..]
```

Notes:
- Only the *last* mention in existing is eligible.
- Works for both plain (`[@X]`) and link (`[@X](...)`) forms by checking terminal character (`]` or `)`).
- Avoids scanning internal earlier mentions—dedup limited to end boundary.

---

## 7. Numeric-Only Suppression Algorithm

Scan the (deduplicated) chunk left → right:

1. Find next `"[@"`.
2. Peek digits until non-digit.
3. If at least one digit read and next char is `']'` (and **not** immediately followed by `'('`):
   - Skip this token entirely (do not append to sanitized output).
4. Otherwise: copy the `"[@"` and continue scanning.

Why avoid parentheses?
- Structured mentions use link syntax (`](...)`); those must remain.

Complexity:
- Single pass, O(n) time, O(1) extra space aside from output buffer.

---

## 8. ASCII Example

Before (model emits stuttered tokens):
```
existing: "We reviewed the plan "
chunk_1:  "[@61]"
chunk_2:  "[@61] and finalized milestones."
```

Processing:
1. Append `chunk_1`: existing ends with `"[@61]"`.
2. When `chunk_2` arrives, boundary dedup trims leading `"[@61]"`.
3. Numeric-only suppression removes first `"[@61]"` in `chunk_1`.
4. Final visible text: `"We reviewed the plan and finalized milestones."`

---

## 9. Edge Cases

| Case | Handling |
|------|----------|
| Duplicate mention separated by space (`"[@42] [@42]"`) | Not collapsed (second is not at chunk boundary). |
| `[ @42]` with space after `[` | Not recognized (strict pattern `[ @` without internal spaces). |
| `[ @0042 ]` padded digits | Not suppressed (spaces break numeric-only pattern). |
| `[@42](detail)` | Preserved (link form). |
| Chunk starts with newline then duplicate | Newline prevents `starts_with(tail)` match; no dedup. |
| Multi-mention chunk `[@1][@2][@2]` | Suppression removes `[@1]` & `[@2]` if numeric-only, ALL numeric-only tokens. Dedup only applies to first if boundary duplicate. |
| Very long existing text with no trailing `[@` | No dedup attempt. |

---

## 10. Non-Goals

- De-duplicating tokens inside the same chunk.
- Semantic validation of whether number had meaning.
- Configurable pattern lists (only numeric-only for now).
- Regex-based multi-token condensation.

---

## 11. Rationale for Simplicity

- Streaming granularity differs per provider; minimal logic prevents false positives.
- Numeric-only tags are the most common meaningless artifacts in tests.
- Avoid complex backtracking to keep UI responsive.

---

## 12. Extensibility Hooks

Potential future options:
- Config toggles: `suppress_numeric_mentions: bool`, `deduplicate_mentions: bool`.
- Minimum length threshold (e.g., suppress only if digits length ≤ 3).
- Maintain a suppression counter for telemetry.
- Support pattern allowlist (e.g., preserve `[@RFC1234]` style even if numeric prefix).

---

## 13. Telemetry (Suggested)

Track:
- `suppressed_mentions_count`
- `deduplicated_mentions_count`
- `total_stream_chunks_processed`

Add lightweight counters incremented during event handling. Expose via debug panel or log once per session.

---

## 14. Testing Strategy

Unit tests (pseudo):

1. `test_deduplicate_plain_token_boundary`
   - existing: `"Alpha [@X]"`, chunk: `"[@X]Beta"`
   - expected: appended `"Beta"` only (after suppression if numeric).
2. `test_preserve_structured_link`
   - existing: `"Start "`, chunk: `"[@file.txt](@file:/a/file.txt) continues"`
   - preserved exactly.
3. `test_suppress_numeric_only`
   - chunk: `"[@123] details"`
   - output: `" details"`.
4. `test_no_suppress_alphanumeric`
   - chunk: `"[@R2D2] status"`
   - output unchanged.
5. `test_multiple_numeric_tokens`
   - chunk: `"Intro [@7][@8] end"`
   - output: `"Intro  end"`.
6. `test_no_boundary_dedup_internal_duplicate`
   - existing: `"[@42]"`, chunk: `" text [@42]"` → only numeric suppression for leading token; internal `[ @42 ]` preserved if spaced.

Integration:
- Simulate streaming by feeding incremental chunks: assert final stored message text matches sanitized path.

---

## 15. Re-Implementation Checklist

1. Maintain a pending assistant message accumulator (`String`).
2. On each incoming chunk:
   - Perform boundary dedup step.
   - Run numeric-only suppression pass.
   - Emit sanitized chunk to UI/event stream.
   - Append sanitized chunk to last assistant message text component.
3. Ensure operations are UTF‑8 safe; indices derived from byte positions must align (ASCII assumption holds for pattern tokens).
4. Avoid allocating excessive intermediate strings (reuse buffer capacities).
5. Preserve non-text content (thinking/tool sections) via separate handlers.

---

## 16. Complexity & Performance

- Dedup: O(m) for scanning tail (worst-case substring search via `rfind("[@")`).
- Suppression: O(n) single forward scan.
- Total per chunk: O(m + n); typically small (< a few KB).
- Memory: Additional `sanitized` buffer with capacity = chunk length.

---

## 17. Failure Modes / Safety

- If malformed patterns occur (e.g. unclosed `[@`), the algorithm leaves them untouched.
- No panic paths: all indexing guarded by bounds checks.
- Large chunks still linear-processed; consider truncation upstream for oversized outputs.

---

## 18. ASCII Diagram (Boundary Detail)

```
existing buffer end                        new chunk
------------------                         ---------------
" ... summary [@42]" || "[ @42 ]info"      NO MATCH (spaces)
" ... summary [@42]" || "[@42]info"        MATCH -> remove leading "[@42]"
" ... summary [@file.md](/...)" || "[@file.md](/...)"  MATCH (link) -> remove duplicate
```

Clarification: spaces or altered punctuation break match, intentionally conservative.

---

## 19. Future Enhancements

| Enhancement | Benefit |
|-------------|---------|
| Configurable suppression regex | Wider pattern control |
| Token-level streaming integration | Suppress before surrogate pair assembly |
| Multi-duplicate collapse within chunk | Cleaner output on heavy stutter |
| Semantic mention classification | Avoid suppressing legitimate numeric IDs |
| User toggle in settings | Opt-out for debugging |

---

## 20. Minimal Reference Implementation (Illustrative Pseudocode)

(This pseudocode avoids crate specifics.)

```
fn process_chunk(existing: &mut String, chunk: String) -> String {
    let mut c = chunk;

    // Dedup boundary
    if let Some(start) = existing.rfind("[@") {
        let tail = &existing[start..];
        if (tail.ends_with(']') || tail.ends_with(')')) && c.starts_with(tail) {
            c = c[tail.len()..].to_string();
        }
    }

    // Suppress numeric-only mentions
    let mut out = String::with_capacity(c.len());
    let mut i = 0;
    while let Some(rel) = c[i..].find("[@") {
        let abs = i + rel;
        out.push_str(&c[i..abs]);
        let mut j = abs + 2;
        while j < c.len() && c.as_bytes()[j].is_ascii_digit() {
            j += 1;
        }
        if j > abs + 2 && j < c.len() && c.as_bytes()[j] == b']' {
            // Numeric-only mention; skip
            i = j + 1;
            continue;
        } else {
            out.push_str("[@");
            i = abs + 2;
        }
    }
    out.push_str(&c[i..]);
    out
}
```

---

## 21. Logging (Optional)

Add debug logs only when:
- Dedup occurs: `"mention_dedup: token=<tail>"`
- Suppression occurs: `"numeric_mention_suppressed: token=[@<digits>]"`

Guard logs behind a feature flag or verbose mode to avoid spam.

---

## 22. Summary

The streaming mention suppression feature improves readability and user experience by pruning duplicated and meaningless numeric-only mention tokens during assistant message construction—implemented with lightweight, boundary-aware transformations that are safe, deterministic, and easily extensible.

---