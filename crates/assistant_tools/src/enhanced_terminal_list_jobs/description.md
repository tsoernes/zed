# Enhanced Terminal List Jobs Tool

Enumerate all known asynchronous terminal jobs with structured JSON for dashboards, progress tracking, and coordination. This tool complements `enhanced_terminal_async` (for launching jobs) and `enhanced_terminal_job_status` (for inspecting/canceling a specific job), and is ideal for multitasking workflows where multiple long‑running commands run in parallel.

## When to use

Use the list jobs tool when you need:
- A consolidated view of running/finished/canceled jobs.
- Aggregated monitoring across many parallel tasks.
- UI tables or dashboards that show status, runtime, and short previews.
- Lightweight polling to keep the Agent Panel in sync without requesting large outputs.

For detailed inspection or cancellation of a single job, use the job status tool instead.

## Inputs

This tool currently takes an empty object as input:

```json
{}
```

(If filtering/sorting options are added later, they will be documented here. For now, simply call it to get the full list.)

## Structured responses

All responses are JSON strings with a single top‑level field:

- `jobs` — Array of job summaries (one per known job).

Each job summary includes predictable fields:

- `job_id` (string)
- `state` (string: `"running" | "finished" | "canceled"`)
- `exit_code` (number | null): `null` while running or when exit code is not available.
- `success` (boolean): Typically equals `exit_code == 0` when available.
- `truncated` (boolean): Whether the `preview` content is truncated.
- `canceled` (boolean): Whether cancellation was requested/marked.
- `runtime_secs` (number): Elapsed seconds; updates while running, total duration once finished.
- `preview` (string): Short output snippet; capped by the job’s `output_limit`.
- `command` (string): Original command.
- `used_sudo` (boolean): Whether the command ran with elevation.
- `dangerous` (boolean): Whether the command matched a denylisted pattern.
- `started_at` (number): Start time as seconds since UNIX epoch.

### Example response

```json
{
  "jobs": [
    {
      "job_id": "enhterm-job-7",
      "state": "running",
      "exit_code": null,
      "success": false,
      "truncated": false,
      "canceled": false,
      "runtime_secs": 12,
      "preview": "building target x...",
      "command": "cargo build --workspace",
      "used_sudo": false,
      "dangerous": false,
      "started_at": 1730000100
    },
    {
      "job_id": "enhterm-job-8",
      "state": "finished",
      "exit_code": 0,
      "success": true,
      "truncated": false,
      "canceled": false,
      "runtime_secs": 31,
      "preview": "done\n",
      "command": "sleep 1 && echo done",
      "used_sudo": false,
      "dangerous": false,
      "started_at": 1730000080
    }
  ]
}
```

## Examples

List all jobs:
```json
{}
```

Use `job_id` from this list with the job status tool to:
- Poll a specific job’s `state`, `runtime_secs`, and output preview.
- Request `full_output:true` after completion.
- Perform cancellation (`cancel:true`) when needed.

## UI guidance

- Render a table with columns: Job ID, State, Runtime, Preview, Command, Sudo, Dangerous, Started At.
- Provide sort/filter controls:
  - Sort by `state`, `runtime_secs`, or `started_at`.
  - Filter by `state` (e.g., only running jobs).
  - Quick search by `job_id` or fuzzy match on `command`.
- Drill‑down:
  - Click a job row to open a detail view that uses the job status tool.
  - Fetch `full_output` on demand once finished/canceled.
- Refresh cadence:
  - Poll this list at modest intervals (e.g., 1–3 seconds) to keep the UI responsive without excessive traffic.
  - Prefer smaller previews and selective full output retrieval to minimize payloads.

## Best practices

- Use this tool to coordinate many parallel tasks; rely on the job status tool for heavy outputs and control actions.
- Avoid large, frequent payloads: short previews are sufficient for dashboards, fetch `full_output` only when necessary.
- Surface `used_sudo` and `dangerous` flags prominently to aid operator awareness and safety reviews.
- Combine with domain/context tools (e.g., MCP servers) to annotate jobs with business‑level context in the UI.

## Summary

The list jobs tool provides an efficient, structured snapshot of all asynchronous terminal jobs. Use it to build responsive, multi‑job dashboards and to orchestrate parallel work. Pair it with the async terminal and job status tools for a complete lifecycle: launch → monitor → control → inspect results.