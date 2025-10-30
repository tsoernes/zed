# Enhanced Terminal Job Status Tool

Query the status of an asynchronous terminal job, optionally cancel it, and (once finished or canceled) retrieve the complete untruncated output. Responses are returned as structured JSON for reliable consumption by the LLM and UI.

Use this tool together with:
- `enhanced_terminal_async` — to start jobs without blocking the conversation.
- `enhanced_terminal_list_jobs` — to enumerate active/finished jobs and build dashboards.

---

## When to use

- Poll a job’s progress without blocking the model’s reasoning.
- Show live runtime and a short output preview in the UI.
- Cancel a misbehaving or unnecessary job (best‑effort; signal‑based on Unix).
- Fetch the full output after a job completes or is canceled.

Avoid calling this tool to start new jobs; instead, launch with `enhanced_terminal_async`.

---

## Inputs

- `job_id` (string, required)
  - Identifier returned by `enhanced_terminal_async`.
- `full_output` (bool, optional, default `false`)
  - When `true` and the job is `finished` or `canceled`, returns the complete output as `full_output`.
  - When the job is still `running`, the tool ignores this flag and returns a `preview` instead.
- `cancel` (bool, optional, default `false`)
  - Best‑effort cancellation request.
  - On Unix, attempts a graceful `SIGTERM`, escalating to `SIGKILL` after ~5 seconds if the process still hasn’t terminated.
  - On non‑Unix platforms, marks the job as canceled as best‑effort (actual process termination may be limited).

---

## States

- `running` — job is currently executing.
- `finished` — job exited normally (exit code may be non‑zero).
- `canceled` — job was canceled (may or may not have produced partial output).
- `not_found` — job id is unknown (expired, pruned, or incorrect id).

---

## Structured responses

All responses are JSON strings with consistent fields.

1) Running (preview mode)
```json
{
  "job_id": "enhterm-job-42",
  "state": "running",
  "exit_code": null,
  "success": false,
  "truncated": false,
  "canceled": false,
  "runtime_secs": 12,
  "preview": "first bytes of combined stdout/stderr (may be empty initially)",
  "used_sudo": false,
  "dangerous": false
}
```

2) Finished (preview)
```json
{
  "job_id": "enhterm-job-42",
  "state": "finished",
  "exit_code": 0,
  "success": true,
  "truncated": false,
  "canceled": false,
  "runtime_secs": 31,
  "preview": "first bytes of output (full output available on request)",
  "used_sudo": false,
  "dangerous": false
}
```

3) Finished (full output)
```json
{
  "job_id": "enhterm-job-42",
  "state": "finished",
  "exit_code": 2,
  "success": false,
  "truncated": true,
  "canceled": false,
  "runtime_secs": 44,
  "full_output": "entire captured output (may be large)",
  "used_sudo": false,
  "dangerous": false
}
```

4) Canceled
```json
{
  "job_id": "enhterm-job-42",
  "state": "canceled",
  "exit_code": null,
  "success": false,
  "truncated": false,
  "canceled": true,
  "runtime_secs": 9,
  "preview": "partial/last known preview or empty",
  "used_sudo": false,
  "dangerous": false
}
```

5) Not found
```json
{ "job_id": "enhterm-job-unknown", "state": "not_found" }
```

Notes:
- `runtime_secs` is elapsed time since start (or total duration if finished). It updates as the job runs.
- `exit_code` is `null` while running or if the platform cannot provide one.
- `success` is generally `exit_code == 0` on supported platforms.
- `preview` is capped by the job’s `output_limit`; `truncated` indicates whether the preview was cut.
- `full_output` is returned only when `full_output:true` is requested and the job is `finished` or `canceled`.

---

## Examples

Poll status:
```json
{ "job_id": "enhterm-job-7" }
```

Fetch full output after completion:
```json
{ "job_id": "enhterm-job-7", "full_output": true }
```

Cancel a running job:
```json
{ "job_id": "enhterm-job-7", "cancel": true }
```

---

## Cancellation semantics

- Unix: send `SIGTERM` first; if the process does not exit within ~5 seconds, send `SIGKILL`.
- Non‑Unix: mark job canceled (process termination may be limited by the OS/runtime).
- Canceled jobs can still expose any captured output via `preview` or `full_output`.

---

## Integration patterns

- With `enhanced_terminal_async`:
  - Launch multiple long‑running operations with `timeout_seconds: 0` to return immediately.
  - Poll each `job_id` with this status tool to update UI badges and runtime.
  - Fetch `full_output:true` only when the job transitions to `finished` or `canceled`.

- With `enhanced_terminal_list_jobs`:
  - Build a consolidated “Jobs” view listing `running/finished/canceled`, `runtime_secs`, `preview`, and `command`.
  - Provide sort/filter controls (e.g., by state, duration, or command).
  - Drill down into a specific job via `job_id` to request full output on demand.

---

## UI guidance

- Render a card per job with:
  - State badge (running/finished/canceled), elapsed time, and preview snippet.
  - Actions: Cancel, View Full Output, Open in terminal view.
- Poll at modest intervals (e.g., 0.5–2.0s) to update status without excessive traffic.
- Defer `full_output` requests until the user expands a job entry or a task completes.

---

## Best practices

- Avoid aggressive polling; use a short interval with backoff if needed.
- Prefer `preview` while running; retrieve `full_output` selectively to minimize payload size.
- If the job fails (`exit_code != 0`), include `full_output` only when relevant for diagnostics.
- Encourage parallel pipelines by combining this tool with `enhanced_terminal_async` and domain/context MCP tools.
- Surface `used_sudo` and `dangerous` flags to help users assess safety and intent.

---

## Summary

`enhanced_terminal_job_status` provides a reliable, structured way to observe, control, and retrieve results from asynchronous terminal jobs. Use it to power responsive UIs and well‑coordinated pipelines where long‑running work proceeds in parallel without blocking the LLM’s reasoning loop.