# Memory Segments (Agent2 Thread Archival)

## 1. Overview

Memory Segments compress earlier conversation history by replacing a contiguous inclusive range of messages with a single summary placeholder message plus metadata. This reduces active token load and keeps recent context uncluttered while preserving high‑level semantics of older exchanges.

Goals:
- Lower prompt size for long threads.
- Retain a concise semantic summary of archived messages.
- Prevent overlapping or duplicate archival of the same message ranges.
- Allow future reconstruction or higher‑level analytics (token usage, counts).

Non‑Goals (current state):
- Partial reconstruction of original message bodies from a segment alone.
- Cross‑thread memory consolidation (handled elsewhere).
- Automatic semantic clustering (ranges are explicit indexes).

---

## 2. Data Structure

```
ThreadMemorySegment {
  id: u64                         // Monotonically increasing per thread.
  start: usize                    // Inclusive message index (0-based).
  end: usize                      // Inclusive message index (0-based).
  summary: SharedString           // Condensed description of archived messages.
  message_char_count: usize       // Sum of characters of original messages.
  message_count: usize            // Number of messages archived (end - start + 1).
  stored_epoch_ms: u128           // Timestamp of archival (ms since epoch).
  placeholder_char_count: usize   // Length of summary placeholder message.
  message_token_count: usize      // Total tokens (captured at archive time).
  // (Additional fields may be present; above are the core semantics.)
}
```

Constraints / Guarantees:
- `start <= end`
- `end < thread.messages.len()` at creation time
- No overlapping range with any existing segment
- `summary` is trimmed, normalized (excess whitespace collapsed), length‑capped (e.g. ~96 chars, ellipsis applied if exceeded)
- `id` uniqueness scoped to the thread (increment from an internal counter)

---

## 3. Lifecycle & State Transitions

### 3.1 Before Archival
The thread maintains a vector of messages:
```
Index:   0    1    2    3    4    5    6    7    8    9
Role:   U    A    U    A    U    A    U    A    U    A
```

### 3.2 Archival Request
Caller invokes:
```
store_memory_segment_with_summary(start=0, end=7, custom_summary=Some("Setup & preliminary Q/A"))
```
Validation steps:
1. Check index bounds.
2. Check `start <= end`.
3. Verify no existing segment overlaps `[start, end]`.
4. Produce normalized summary (custom or auto).
5. Compute stats (char count, token count).
6. Create `ThreadMemorySegment`.
7. Replace original messages `[0..=7]` with a single placeholder message:
   - Placeholder message role: Assistant (or neutral) depending on implementation.
   - Placeholder content: summary string.

### 3.3 After Archival
Messages vector visually:
```
Index:   0           1    2
Role:   (SEGMENT)    U    A
```
Where original indices 0..7 collapsed into index 0; original 8 becomes new index 1, original 9 becomes index 2.

### 3.4 Subsequent Archival
A future archival cannot target a range intersecting `[0..7]` since it is now represented by a single placeholder (segment boundary). Further segmentation would typically start after the placeholder (e.g. new older range once more messages accumulate).

---

## 4. Diagrams

### 4.1 Range Replacement

```
Before:
+----+----+----+----+----+----+----+----+----+----+
| M0 | M1 | M2 | M3 | M4 | M5 | M6 | M7 | M8 | M9 |
+----+----+----+----+----+----+----+----+----+----+

Archive [0..7]

After:
+---------------+----+----+
|   SEGMENT 0   | M8 | M9 |
+---------------+----+----+
(segment holds summary + stats for M0..M7)
```

### 4.2 Metadata Relationships

```
Thread
  ├─ messages (Vec<Message>)
  ├─ memory_segments (Vec<ThreadMemorySegment>)
  └─ memory_next_id (u64 counter)

ThreadMemorySegment
  ├─ id
  ├─ range [start, end]
  ├─ summary
  ├─ counts (chars, tokens, message_count)
  └─ timestamps
```

### 4.3 Validation Flow

```
[Request] --> [Validate Indices] --> [Check Overlap] --> [Prepare Summary] --> [Compute Stats]
       \                                                             /
        ------------------[On error: return Err(..)]---------------
                      |
                [Create Segment]
                      |
             [Replace Messages]
                      |
               [Push Segment]
```

---

## 5. Algorithmic Details

### 5.1 Overlap Detection
Pseudo:
```
for seg in memory_segments:
    if !(end < seg.start || start > seg.end):
        return Err("range overlaps existing segment")
```
Rationale: Fast linear scan is adequate at small segment counts (segmentation infrequent). If segments grow large, a balanced interval tree could be introduced.

### 5.2 Summary Normalization
Steps:
1. If `custom_summary` provided and not empty post-trim:
   - Trim leading/trailing whitespace.
   - Replace internal sequences of whitespace (including newlines) with single spaces.
   - Enforce max length (truncate and append `…` if needed).
2. Else auto-generate summary:
   - Heuristic: first N chars of first message + possibly role markers + ellipsis.
   - Could incorporate semantic summarization model (future extension).

### 5.3 Placeholder Message Creation
- Role: Assistant (common choice to avoid user confusion).
- Content: summary string.
- Token estimate: approximate via same estimator used for normal messages (or characters → tokens heuristic).

### 5.4 Token & Character Counting
Aggregate:
```
message_char_count = Σ len(m.content)
message_token_count = Σ estimate_tokens(m.content)
placeholder_char_count = len(summary)
```
Token estimator: qualifies approximate maximum context cost; exact counts can be refined by a model-specific tokenizer later.

---

## 6. Prompt Construction Interaction

When building a model request:
- Placeholder summary message is treated as a single message (reducing count).
- Exact archived messages are omitted.
- If downstream logic needs more detail, it can look up segment metadata to potentially reconstruct a richer meta-summary (future: multi-tier summarization).

---

## 7. Error Handling

Returning early with descriptive `Err(anyhow!(..))` for:
- Invalid indices (`start > end` or `end >= messages.len()`).
- Overlapping range with existing segment.
- Internal inconsistency (e.g. message vector mutated mid‑archival — unlikely on single foreground thread).

No panics; invariants enforced before mutation.

---

## 8. Reimplementation Checklist

1. Add `ThreadMemorySegment` struct with required fields.
2. Extend `Thread`:
   - Fields: `memory_segments: Vec<ThreadMemorySegment>`, `memory_next_id: u64`.
3. Implement:
   - `store_memory_segment(start, end, cx) -> Result<u64>`
   - `store_memory_segment_with_summary(start, end, custom_summary, cx) -> Result<u64>`
4. Overlap validator (linear scan).
5. Summary normalization function.
6. Stats aggregation function.
7. Replacement logic:
   - Remove slice `[start..=end]`.
   - Insert placeholder message at `start`.
8. Update any derived counts or cached token usage if present (optional).
9. Notify UI / observers via context (`cx.notify()`).
10. Add tests:
    - Successful archival.
    - Overlap rejection.
    - Out-of-bounds indices.
    - Custom summary truncation.
    - Consecutive archival operations adjusting indices correctly.

---

## 9. Testing Strategy

### 9.1 Unit Tests
- Valid range archival produces exactly one placeholder and correct segment metadata.
- Overlapping second archival fails.
- Custom summary longer than limit is truncated.

### 9.2 Property Tests (Optional)
- For randomized message lengths, `message_char_count` equals sum of originals after archival.

### 9.3 Integration Tests
- Build a thread, append N messages, archive early portion, ensure model prompt builder uses fewer messages in subsequent request.

---

## 10. Edge Cases

| Case | Behavior |
|------|----------|
| start == end | Archives a single message (segment `message_count = 1`). |
| All messages archived except latest | Remaining set still valid; further segmentation limited to new unarchived messages. |
| Empty thread | Archival disallowed (index check fails). |
| Summary identical to existing placeholder content | Allowed (different segment id). |
| Rapid consecutive calls with same indices | First succeeds, second fails (overlap). |

---

## 11. Performance Considerations

- Segment creation is O(k) for range removal and insertion; acceptable given foreground thread and relatively small message lists.
- Overlap detection O(s) where s = number of segments (expected small).
- No extra indexing; complexity dominated by string length summations.
- If segment counts grow, consider maintaining a sorted, non-overlapping invariant (already enforced) enabling binary search for overlap tests.

---

## 12. Future Enhancements

| Enhancement | Description |
|-------------|-------------|
| Multi-tier summaries | Recursive summarization when segment size itself becomes large. |
| Semantic boundaries | Use topic change detection to propose archival ranges automatically. |
| Token-based thresholds | Trigger archival on exceeding token limits rather than manual call. |
| Partial restoration | Expanding a segment to retrieve original messages on demand. |
| Interval tree | Efficient overlap checks at scale. |

---

## 13. ASCII Flow for Auto-Suggestion (Future)

```
[Append Message] --> [Update total tokens] --> (tokens > limit?)
                                   |
                                   +--> [Find earliest unarchived window W]
                                           |
                                           +--> [Auto summarize W]
                                           +--> [Replace with segment]
```

---

## 14. Summary

Memory segments permit strategic compression of long agent conversations by replacing historical ranges with compact summaries plus quantitative metadata (char/token counts). Enforcement of non-overlapping intervals and careful summary normalization preserves correctness, while straightforward data structures keep the system easy to reason about and extend.

Reimplementation requires only a focused set of primitives (range validation, summary generation, metadata aggregation, vector replacement) and integrates cleanly with existing thread update & UI notification patterns.

---