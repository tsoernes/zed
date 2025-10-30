# Enhanced Terminal Tool Documentation

The enhanced terminal tool provides a controlled, non-interactive shell execution interface for the agent and UI layers. It focuses on safe, auditable command runs, lightweight job introspection, and integration with conversation context without spawning runaway processes.

---

## 1. Objectives

- Execute short-lived terminal commands with combined stdout + stderr capture.
- Prevent accidental long-running or interactive processes (e.g. servers, editors).
- Expose task/job metadata for monitoring / cancellation (future extension).
- Provide outputs suitable for injection into agent replies or downstream tools.
- Preserve ordering of interleaved stdout/stderr writes.
- Enforce a predictable, minimal API surface for LLM tool usage.

---

## 2. High-Level Architecture

```
+------------------+        +---------------------+
|   Agent / UI     | --->   |  Enhanced Terminal   |
+------------------+        +----------+----------+
                                     |
                                     v
                            +---------------------+
                            |  Execution Sandbox  |
                            |  (foreground spawn) |
                            +----------+----------+
                                       |
                                       v
                                +-------------+
                                |  Output Log |
                                +-------------+
```

- Calls originate from agent (tool use) or UI triggers.
- The sandbox creates a scoped task future performing the command.
- Output is streamed internally then surfaced as a single collected string (or chunked, future enhancement).
- Job metadata retained for listing (optional enhancement path).

---

## 3. Data Flow (Sequence Diagram)

```
Agent/User Action
    |
    | (1) Request: run_command("git status")
    v
EnhancedTerminalTool
    |
    | (2) Validate command (reject interactive / indefinite)
    |
    | (3) Spawn execution future
    v
Foreground Executor
    |
    | (4) Run process, collect stdout/stderr
    |
    v
EnhancedTerminalTool
    |
    | (5) Normalize output (UTF-8, truncate if > limit)
    |
    | (6) Return JSON envelope { ok: true, output: "<joined>" }
    v
Agent / UI renders result
```

Failure path: At (2) or (4) errors are captured and returned as `{ ok: false, error: "<message>" }`.

---

## 4. Command Validation Strategy

Before spawning, the tool SHOULD enforce:

| Check | Purpose |
|-------|---------|
| Disallow known long-lived commands (e.g. `npm start`, `cargo watch`) | Prevent blocking thread |
| Reject explicit shell loops (`while`, `for` without timeout) | Avoid infinite execution |
| Reject interactive programs (`vim`, `nano`, `python` REPL) | Prevent hung sessions |
| Enforce maximum output size (e.g. 64KB) | Protect memory / UI |
| Enforce maximum runtime (e.g. 15s) | Guard against runaway tasks |

When a disallowed pattern is detected: return an error envelope instead of spawning.

---

## 5. API Surface (Conceptual)

All method calls accept snake_case JSON or minimal argument forms.

```
run_command:
  Input: { "command": "git status" }
  Output:
    { "ok": true,
      "command": "git status",
      "exit_code": 0,
      "output": "<joined stdout+stderr>",
      "duration_ms": 1234 }

list_jobs:
  Input: {}
  Output:
    { "ok": true,
      "jobs": [
        { "job_id": "...", "command": "...", "state": "Running|Succeeded|Failed", "started_at": "...", "duration_ms": null }
      ] }

job_status:
  Input: { "job_id": "..." }
  Output: { "ok": true, "job": { ...same fields plus maybe partial output... } }
```

(Actual implementation can start smaller, omitting job listing if not yet required.)

---

## 6. Internal Structures (Suggested)

```
struct TerminalJob {
    job_id: Uuid,
    command: String,
    started_at: Instant,
    state: JobState,
    output_buffer: String,
    exit_code: Option<i32>,
    duration: Option<Duration>,
}

enum JobState {
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}
```

Jobs kept in a `HashMap<Uuid, TerminalJob>` guarded by foreground-only mutations (single-threaded safety). Large outputs truncated early with a suffix marker `"...<truncated>"`.

---

## 7. Output Normalization

Steps:
1. Read interleaved stdout/stderr lines; append sequentially.
2. Attempt UTF-8 decode; replace invalid sequences with `�`.
3. Limit size (streaming early termination).
4. Strip trailing ANSI escape codes (optional).
5. Return final string.

Pseudo-flow:

```
let mut buf = String::new();
for chunk in stream {
    if buf.len() > MAX {
        buf.push_str("\n...<truncated>");
        break;
    }
    buf.push_str(&chunk);
}
```

---

## 8. Error Handling

- Process spawn failure ⇒ `{ ok: false, error: "spawn error: ..." }`
- Timeout ⇒ kill process, set state Failed, partial output preserved.
- Validation rejection ⇒ `{ ok: false, error: "disallowed command" }`
- Unexpected IO error ⇒ log + error envelope.

No panics; always propagate `Result`.

---

## 9. Security / Safety Considerations

| Concern | Mitigation |
|---------|------------|
| Arbitrary file modifications | User privilege boundary; consider allowlist/denylist |
| Resource exhaustion (large output) | Output truncation + timeouts |
| Shell injection | Caller (agent) is trusted; future enhance by tokenizing and validating argv |
| Permissions escalation | Avoid `sudo`; if required, route through secured askpass flow (not default) |

---

## 10. Integration With Agent Messages

Typical usage:
1. User asks: “Show the git diff.”
2. Agent tool call `run_command {"command":"git diff --name-only"}`.
3. Result returned and inserted into assistant message as a code block or inline text.
4. If output truncated, assistant adds clarification (“Output truncated to 64KB.”).

---

## 11. Caching & Deduplication (Optional Future)

Repeated identical commands with unchanged working directory could be cached (fingerprint: `sha256(command + cwd + env subset)`). Current design: *no cache* to keep semantics transparent.

---

## 12. Sequence With Cancellation (Future Extension)

```
User triggers long command
   |
   v
run_command -> job_id returned early (streaming variant)
   |
User cancels (job_cancel job_id)
   |
Foreground: sends SIGTERM -> updates state Cancelled
   |
job_status => { state: "Cancelled", partial_output: ... }
```

---

## 13. Limitations

- No interactive stdin piping.
- No environment variable customization per call (could add).
- No streaming partial output back to user yet (only final snapshot).
- No per-project concurrency constraints (global queue only).

---

## 14. ASCII Component Diagram (Detailed)

```
+---------------------------------------------------------------+
| EnhancedTerminalTool                                          |
|  - jobs: HashMap<Uuid, TerminalJob>                           |
|  - run_command(cmd)                                           |
|       validate(cmd)                                           |
|       spawn_process(cmd) -> handle                           |
|       collect_output(handle)                                  |
|       finalize(job)                                           |
|                                                               |
|  - list_jobs()  -> Vec<JobSummary>                            |
|  - job_status(id) -> TerminalJob (sanitized)                  |
+-------------------------+-------------------------------------+
                          |
                          v
                  +---------------+
                  | Process (OS)  |
                  | stdout/stderr |
                  +-------+-------+
                          |
                          v
                  +---------------+
                  | Output Buffer |
                  +---------------+
```

---

## 15. Reimplementation Checklist

On a fresh upstream main:

1. Create `enhanced_terminal` module / crate.
2. Define `TerminalJob`, `JobState`.
3. Implement `run_command` (spawn + collect).
4. Add validation helpers (deny patterns).
5. Add output normalization (size limit, UTF-8 restore).
6. Implement `list_jobs`, `job_status` (optional if jobs stored).
7. Wrap all responses in JSON envelopes with `ok` flag.
8. Integrate with agent tool registry (snake_case names).
9. Add tests:
   - Simple success (`echo hello`)
   - Disallowed command (`npm start`)
   - Truncation (generate > limit bytes)
   - Timeout (sleep longer than allowed)
10. Document usage and failure envelopes.

---

## 16. Example JSON Envelopes

Success:
```
{
  "ok": true,
  "command": "echo hello",
  "exit_code": 0,
  "output": "hello\n",
  "duration_ms": 12
}
```

Failure (validation):
```
{
  "ok": false,
  "error": "disallowed command pattern: 'cargo watch'"
}
```

Timeout:
```
{
  "ok": false,
  "error": "timeout exceeded (15000 ms)"
}
```

---

## 17. Future Enhancements

| Feature | Description |
|---------|-------------|
| Streaming | Emit incremental output events for very large commands |
| Structured Parsing | Detect JSON output and return parsed variant automatically |
| Env Profiles | Allow selecting predefined environment sets (e.g., with proxies) |
| Sandboxing | Per-project chroot/container isolation |
| Rate Limits | Per-user command throughput throttling |
| Metrics | Track aggregate usage (count, avg duration, failure rate) |

---

## 18. Design Principles

- Predictability: identical inputs yield deterministic envelopes.
- Safety first: reject suspicious / long-running patterns.
- Transparency: never hide partial failures; propagate errors explicitly.
- Extensibility: additional backend (e.g., remote execution) can implement same trait.
- Minimal mutation surface: jobs only updated by the execution path, not external arbitrary edits.

---

## 19. High-Level Pseudocode

```
fn run_command(input: RunCommandInput) -> Json {
    validate(&input.command)?;
    let job = TerminalJob::new(input.command.clone());
    jobs.insert(job.id, job.clone());
    let output = execute_collect(&input.command, TIMEOUT, OUTPUT_LIMIT)?;
    job.finish(output.exit_code, output.content);
    Ok(json!({ "ok": true, ... }))
}

fn execute_collect(cmd: &str, timeout, limit) -> Result<CollectedOutput> {
    spawn process;
    while running and before timeout {
        read chunk;
        append chunk;
        if size > limit { truncate; break; }
    }
    return CollectedOutput { exit_code, content }
}
```

---

## 20. Testing Notes

- Use fast commands for deterministic test runtime.
- Mock timeouts by spawning `sleep` with duration > configured timeout.
- Simulate large output by printing repeated lines (e.g., `yes line | head -n 100000`).

---

End of enhanced terminal documentation.