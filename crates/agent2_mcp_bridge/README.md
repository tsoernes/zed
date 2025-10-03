# agent2_mcp_bridge

Exposes Zed’s new agent2 conversation context–management tools (`list_history`, `memory`, `rewrite_history`) through a lightweight JSON‑RPC / MCP–style server so external clients (or other processes) can programmatically inspect and compact conversation history.

> Status: SKELETON (scaffolding committed). Core bootstrap (Project + Thread + real event stream wiring) still TODO.  
> This README documents the intended end state and the steps required to finish the bridge.

---

## 1. Motivation

Large, evolving conversations hit model context/token limits. The editor now ships internal tools that:

| Tool            | Purpose                                                                 |
|-----------------|-------------------------------------------------------------------------|
| `list_history`  | Enumerate messages with stable indices & previews.                      |
| `memory`        | Archive a contiguous range (persist + replace with a compact marker).   |
| `rewrite_history` | Remove or summarize a range into a single placeholder marker.        |

Internally these are `AgentTool` implementations attached to a `Thread`. Outside the editor we want (a) automation, (b) reproducible compression pipelines, (c) potential backend orchestration of multi‑turn threads. The bridge exposes only a *safe, curated* subset—not the full file system or project mutators.

---

## 2. High‑Level Architecture

```
+---------------------------+         JSON-RPC (stdin/stdout)          +-------------------+
| External Client / Runner  | <--------------------------------------> | agent2_mcp_bridge |
+---------------------------+                                          +---------+---------+
                                                                          (creates)
                                                           +----------------------+
                                                           |   gpui App Runtime  |
                                                           +----------+----------+
                                                                      |
                                                                      v
                                                           +----------------------+
                                                           |  Project / Context   |
                                                           +----------+----------+
                                                                      |
                                                                      v
                                                           +----------------------+
                                                           |  Thread (agent2)     |
                                                           |  + registered tools  |
                                                           +----------+----------+
                                                                      |
                                                            ToolCallEventStream
                                                                      |
                                                                      v
                                                           +----------------------+
                                                           | list_history / ...   |
                                                           +----------------------+
```

---

## 3. Current Implementation Snapshot

| Aspect                    | Implemented? | Notes |
|---------------------------|--------------|-------|
| Binary crate & deps       | ✅           | `Cargo.toml` present. |
| Tool export adapter (`export.rs`) | ✅  | Provides schema derivation + invoker. Needs real event stream provider. |
| JSON-RPC loop (initialize / tools/list / tools/call) | ✅ | Simplified line-by-line parsing. |
| Thread / Project bootstrap | ❌          | Placeholder returns empty registry. |
| Real `EventStreamProvider` | ❌          | Placeholder panics if invoked. |
| Registration of target tools | ❌       | Stub (`register_context_tools`) present. |
| Auxiliary message injection tools | ❌   | Recommended for practical usage. |
| Streaming incremental updates | Deferred | Could map internal updates to `tools/call/update`. |

---

## 4. Roadmap / TODO (Suggested Order)

1. Project + Thread Bootstrap  
   - Create a minimal in-memory `Project`.
   - Instantiate supporting entities: `ProjectContext`, `ContextServerRegistry`, `Templates`.
   - Construct a `Thread` and call `add_default_tools(...)`.

2. Extract & Adapt Tools  
   - Fetch the `Arc<...>` instances of `list_history`, `memory`, `rewrite_history`.
   - Wrap each with `adapt_tool(...)` and place into `BridgeState.tools`.
   - Mark `state.ready = true`.

3. Implement `EventStreamProvider`  
   - Build a real `ToolCallEventStream` (mirroring how the editor constructs it—requires a `ThreadEventStream` or a no-op variant).
   - Translate tool progress updates to either logs or future JSON-RPC notifications.

4. Add Message Injection Tools (optional but highly useful):  
   - `add_user_message { text }`  
   - `add_assistant_message { text }`  
   - `show_messages { start, limit }` (thin convenience wrapper around `list_history` but without the extra JSON table formatting overhead).

5. Error Mapping / UX  
   - Map argument errors (deserialization / range validation) → JSON-RPC error code `400`.  
   - Internal failures (IO/persistence) → `500`.  
   - Include `data` with structured context when safe.

6. Streaming Support (Optional)  
   - Introduce `notifications/toolUpdate` events with partial content.
   - Accumulate final result for existing `tools/call` reply.

7. Persistence (Optional)  
   - If long-lived, persist archived memory handles to stable storage (the underlying `memory` tool already uses KEY_VALUE_STORE; ensure the store is configured in this headless mode).

---

## 5. Bootstrapping a Thread (Conceptual Steps)

Pseudo-code (details depend on existing crate APIs):

```rust
let mut app = App::new();

let project = app.update(|cx| {
    // Create or open a minimal project (cwd or temp dir)
    create_minimal_project(cx)
})?;

let project_context = app.update(|cx| create_project_context(project.clone(), cx))?;
let context_server_registry = app.update(|cx| create_context_server_registry(cx))?;
let templates = load_templates(); // existing Templates handle
let model = select_default_model(); // Option<Arc<dyn LanguageModel>>

let thread_entity = app.update(|cx| {
    let mut thread = Thread::new(
        project.clone(),
        project_context.clone(),
        context_server_registry.clone(),
        templates.clone(),
        model,
        cx
    );
    thread.add_default_tools(environment_adapter(), cx);
    cx.new(|_| thread)
});

register_context_tools(thread_entity, &mut app)?;
```

Key objects you have to satisfy:
- `Project`
- `ProjectContext`
- `ContextServerRegistry`
- `Templates` (may reuse default)
- Optional: selected model to allow certain token usage features (not strictly required for these tools unless you later add auto-summarization inside `rewrite_history`).

---

## 6. Event Stream Provider Implementation

Implement something like:

```rust
struct BridgeEventStreamProvider {
    thread_stream: ThreadEventStream, // or a wrapper
    fs: Option<Arc<dyn Fs>>,          // Acquire from project
}

impl EventStreamProvider for BridgeEventStreamProvider {
    fn new_stream(&self, tool_name: &str) -> ToolCallEventStream {
        let tool_use_id = LanguageModelToolUseId::from(format!("bridge-{tool_name}-{}", Uuid::new_v4()));
        ToolCallEventStream::new(tool_use_id, self.thread_stream.clone(), self.fs.clone())
    }
}
```

If you cannot (or do not want to) surface intermediate updates:
- Provide a lightweight `ThreadEventStream::noop()` variant (if available) or re-implement a dummy that discards events.

---

## 7. JSON-RPC Methods (Current Minimal Set)

| Method         | Direction | Description |
|----------------|-----------|-------------|
| `initialize`   | Request   | Returns protocol + capability summary. |
| `tools/list`   | Request   | Lists exported tools with JSON Schemas. |
| `tools/call`   | Request   | Invokes a named tool, returning final `content`. |

Sample request flow (line-delimited):
```
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}
{"jsonrpc":"2.0","id":2,"method":"tools/list"}
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_history","arguments":{"start":0,"limit":20}}}
```

Sample success response (conceptual):
```
{
  "jsonrpc":"2.0",
  "id":3,
  "result":{
    "content":[
      {"type":"text","text":"History indices 0..5 of 6 ..."},
      {"type":"json","json":{"total_messages":6,"range_start":0,"range_end":5,"messages":[...]}}
    ]
  }
}
```

Error (unknown tool):
```
{
  "jsonrpc":"2.0",
  "id":10,
  "error":{"code":400,"message":"Unknown tool 'foo'"}
}
```

---

## 8. Tool Usage Patterns

1. Enumerate candidate ranges:
   - Call `list_history` with small `limit` windows to inspect earlier segments.

2. Archive large obsolete spans:
   - Call `memory` with `{"operation":"store","start_index":X,"end_index":Y}`.
   - Store the returned handle for future `load`.

3. Summarize mid-range region:
   - Call `rewrite_history` with `{"start_index":A,"end_index":B,"strategy":"summarize"}`.

4. Retrieve archived content (later):
   - `memory` with `{"operation":"load","memory_handle":"mem://<session>/<uuid>"}`.

---

## 9. Adding Auxiliary Injection Tools (Recommended)

These are simple to implement and make the bridge far more usable:

| Tool Name            | Input Schema                          | Action |
|----------------------|----------------------------------------|--------|
| `add_user_message`   | `{ "text": string }`                   | Pushes a user message. |
| `add_assistant_message` | `{ "text": string }`               | Pushes an assistant message (useful for test harness seeding). |
| `truncate_history` (optional) | `{ "after_index": number }` | Drops messages after index (reset tail). |

All can be implemented as lightweight `AgentTool` wrappers or direct RPC methods (consistent with design—prefer tools for discoverability).

---

## 10. Error Handling Guidelines

| Scenario                              | Code | Message Pattern |
|---------------------------------------|------|-----------------|
| Bad JSON / argument shape             | 400  | "Failed to deserialize input ..." |
| Out-of-range indices                  | 400  | Propagate underlying tool error. |
| Unknown tool                          | 400  | "Unknown tool 'name'" |
| Thread not yet ready                  | 503  | "Bridge not yet fully initialized..." |
| Internal panic / IO failure           | 500  | "Internal error: ..." |

> Consider adding `data` field with a machine-readable sub-structure later.

---

## 11. Security & Safety Considerations

- Scope: This bridge should NOT (by default) expose file system mutation tools. Only the context tools.
- Memory Growth: `memory` tool persists archived ranges. Put quotas on number of memory handles or total stored bytes per session.
- Summarization Integrity: Summaries are lossy; warn downstream systems not to treat placeholders as semantic equivalents.
- Sandboxing: If you later expose file tools, run the process in a restricted environment (chroot/container) or enforce path whitelists.

---

## 12. Performance Considerations

| Concern                  | Mitigation |
|--------------------------|------------|
| Large archived spans     | Use streaming persistence (already asynchronous for KV writes). |
| High-frequency list calls| Add simple in-memory LRU of last serialization segments. |
| JSON schema recomputation| Cache the derived schema per tool at export time (already done). |
| Blocking `pollster::block_on` | Acceptable for quick tools; for longer tasks spawn async + reply later (future work). |

---

## 13. Testing Strategy

1. Unit tests for export layer (schema presence / deserialization failure path).
2. Integration test launching the binary:
   - Feed scripted JSON lines.
   - Validate `tools/list` returns expected tool names.
   - Seed messages, run `list_history`, then `memory` store → verify placeholder appears.
3. Round-trip `memory` load handles.
4. Fuzz invalid argument shapes (e.g., negative indices, missing required fields).

---

## 14. Extensibility

Future improvements:
- Tool capability metadata (idempotent? destructive?).
- Token budget advisory tool (estimates next-turn usage, suggests compression).
- Multi-thread session manager (list active sessions; select one).
- Event notifications for tool progress / memory store completion.

---

## 15. Minimal Completion Checklist

| Item | Done? | Notes |
|------|-------|-------|
| Thread bootstrap | ☐ | Implement real creation path. |
| Register tools | ☐ | Use `adapt_tool` for 3 target tools. |
| Real EventStreamProvider | ☐ | Provide meaningful (even if silent) stream. |
| Message injection tools | ☐ | At least `add_user_message`. |
| Error code refinement | ☐ | Use 400/500 distinctions. |
| Basic integration test | ☐ | Validate end-to-end flow. |

---

## 16. Example End-State Session

```
> initialize
< capabilities ...
> tools/list
< ["list_history","memory","rewrite_history","add_user_message"]
> tools/call add_user_message {"text":"We discussed API pagination strategy."}
< success
> tools/call list_history {"start":0,"limit":10}
< shows message at index 0
> tools/call memory {"operation":"store","start_index":0,"end_index":0}
< returns handle mem://<session>/<uuid>
> tools/call list_history {"start":0,"limit":5}
< index 0 now shows [[memory: ...]] marker
```

---

## 17. Contributing

1. Implement missing bootstrap in `bootstrap_bridge_state`.
2. Replace placeholder event stream provider.
3. Submit focused PRs (one logical enhancement per PR).
4. Add or update tests / README sections when adding features.

---

## 18. License

This bridge crate inherits the repository’s overarching licensing terms (AGPL-3.0-or-later for original code unless otherwise stated). Verify compatibility if integrating external crates.

---

## 19. Quick Reference

| File                                      | Purpose |
|-------------------------------------------|---------|
| `src/main.rs`                             | JSON-RPC server skeleton. |
| `src/export.rs`                           | Tool export & schema adapter. |
| `README.md`                               | This document. |

---

### Final Notes

This document intentionally over-specifies the desired final architecture to reduce iteration overhead. Implement the missing pieces incrementally; the export abstraction is already in place to keep the remainder of the work localized to bootstrap and event stream provisioning.

Happy hacking!