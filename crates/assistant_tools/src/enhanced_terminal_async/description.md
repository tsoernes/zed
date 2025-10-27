# Enhanced Async Terminal Tool

Execute shell commands asynchronously with structured responses, optional timeout‑based waiting, and controlled environment propagation. This tool is designed for multitasking and parallel execution: it launches the process in the background and returns promptly so the model can continue working (e.g., calling other tools) without blocking.

Use this tool together with:
- `enhanced_terminal_job_status` to poll state, cancel, or retrieve full output.
- `enhanced_terminal_list_jobs` to enumerate and coordinate multiple jobs.

## When to use

Prefer the async terminal tool when you need one or more of:
- Multitasking / parallel execution (do not block on long operations).
- Immediate continuation of reasoning or other tool calls while a command runs.
- Optional bounded wait (timeout) before falling back to a job id.
- Structured JSON responses (safer/more predictable LLM consumption).
- Fine‑grained control over which environment variables are passed through.

Use a synchronous terminal tool only for fast, single‑shot commands where blocking is acceptable.

## Inputs

- `command` (string, required): Shell snippet to execute. Do not include `cd`; use `cwd`.
- `cwd` (string, default `"."`):
  - `"."` or `""` — resolves to the single worktree root (fails if multiple roots).
  - Absolute path — must exist.
  - Worktree name — resolves to that project root.
- `use_sudo` (bool, default `false`): Prefix with `sudo -S`. Requires host authorization; use only when necessary.
- `shell` (string, optional): Preferred shell name or absolute path. Common values: `bash`, `zsh`, `fish`, `sh`, `dash`, `ksh`, or a full path like `/usr/bin/fish`.
- `output_limit` (integer, optional): Max bytes retained in preview. Defaults: 16KB normal, 256KB when `use_sudo` unless overridden.
- `timeout_seconds` (integer, default `0`):
  - `0` — return immediately with a `job_id` and `state:"running"`.
  - `> 0` — wait up to this many seconds; if the process finishes in time, return a structured result; otherwise return a `job_id` and `state:"running"`.
- `env_whitelist` (array of strings, optional): Names of environment variables to pass through. Variables not present in the host environment are skipped.
- `allow_dangerous` (bool, default `false`): Permit commands that match built‑in or custom denylist patterns. Requires global setting `agent.enhanced_terminal_allow_dangerous=true` to be in effect.

## Structured responses

All responses are JSON strings with predictable fields:

1) Immediate return (timeout_seconds = 0)
```json
{ "job_id": "enhterm-job-42", "state": "running" }
```

2) Completed within timeout
```json
{
  "job_id": "enhterm-job-42",
  "state": "finished",
  "exit_code": 0,
  "success": true,
  "truncated": false,
  "runtime_secs": 3,
  "output": "first bytes of combined stdout/stderr (preview)",
  "used_sudo": false,
  "dangerous": false
}
```

3) Still running after timeout
```json
{ "job_id": "enhterm-job-42", "state": "running" }
```

4) Not found (rare; indicates registry mismatch)
```json
{ "job_id": "enhterm-job-42", "state": "not_found" }
```

Notes:
- `output` is a preview capped by `output_limit`. Full output retrieval happens via `enhanced_terminal_job_status` with `full_output:true` after completion.
- `runtime_secs` is included on completion in async result. While still running, use the status tool to obtain `runtime_secs`.

## Examples

Start async and continue immediately:
```json
{
  "command": "sleep 30 && echo done",
  "cwd": ".",
  "timeout_seconds": 0
}
```

Start async and wait up to 5s before returning a job id:
```json
{
  "command": "long_task.sh",
  "timeout_seconds": 5
}
```

Use a specific shell and limited preview:
```json
{
  "command": "for f in *.rs; echo $f; end",
  "shell": "fish",
  "output_limit": 65536,
  "timeout_seconds": 0
}
```

Pass selected environment variables:
```json
{
  "command": "ci_build --project $PROJECT_ID --token $CI_TOKEN",
  "env_whitelist": ["PROJECT_ID", "CI_TOKEN"],
  "timeout_seconds": 0
}
```

Elevated command with justification and bounded wait:
```json
{
  "command": "apt-get update",
  "use_sudo": true,
  "timeout_seconds": 10,
  "output_limit": 262144
}
```

## Lifecycle coordination

Use these complementary tools to manage job lifecycles and UI:

- `enhanced_terminal_job_status`:
  - Input: `job_id`, `full_output` (bool), `cancel` (bool).
  - Returns structured JSON with `state`, `exit_code`, `success`, `truncated`, `canceled`, `runtime_secs`, and either `preview` or `full_output`.
  - Cancellation (Unix): attempts SIGTERM then escalates to SIGKILL after ~5s.

- `enhanced_terminal_list_jobs`:
  - Returns `{ "jobs": [ ... ] }`, each entry containing `job_id`, `state`, `exit_code`, `success`, `truncated`, `canceled`, `runtime_secs`, `preview`, `command`, `used_sudo`, `dangerous`, `started_at`.

Recommended pattern:
1. Launch with `enhanced_terminal_async` (timeout 0) to immediately return `job_id`.
2. Poll status periodically to update UI badges (running/finished/canceled), `runtime_secs`, and preview.
3. Request `full_output:true` after completion to fetch complete logs on demand.
4. Use job listing for a consolidated dashboard and coordination.

## Safety

Dangerous commands are blocked by default unless both `allow_dangerous:true` and the global setting `agent.enhanced_terminal_allow_dangerous:true` are in effect. Built‑in patterns include:
- `rm -rf /`
- `mkfs`
- `:(){:|:&};:` (fork bomb)
- `dd if=`
- `shutdown -h`
- `reboot`
- `chmod 777 /`
- `chown root:`

You can add custom denylist patterns via `agent.enhanced_terminal_denylist` and disable the built‑ins via `agent.enhanced_terminal_disable_default_denylist`.

## Best practices

- Prefer async for long or IO‑heavy tasks to keep the agent responsive.
- Keep `timeout_seconds` small when you want the model to continue quickly; rely on status polling rather than waiting.
- Use `env_whitelist` for least‑privilege environment propagation.
- Avoid fetching `full_output` automatically; use preview then retrieve full logs selectively.
- Justify `use_sudo` and avoid destructive commands unless intentionally required.
- For large outputs, increase `output_limit` cautiously and rely on `full_output` post‑completion when necessary.

## Summary

`enhanced_terminal_async` is the recommended tool for parallel execution pipelines. It launches commands in the background, returns structured responses, and integrates seamlessly with job status and listing tools. Use it to keep the LLM’s reasoning loop unblocked while long‑running work proceeds in parallel.