Execute shell commands with advanced capabilities including optional sudo elevation, flexible working directory selection (inside or outside project roots), custom shell choice, larger output limits, timeout-based detachment, detachable background execution with status polling, real signal-based cancellation, full output retrieval, job listing, and incremental streaming preview of long-running output.



Key features:
- Run commands in any directory: pass an absolute path (e.g. /var/log) or a project worktree name, or use "." when there is exactly one worktree.
- Optional sudo: set use_sudo: true to prefix the command with sudo -S (authorization required). Only use when strictly necessary and prefer read‑only or least‑privilege operations.
- Custom shell: choose a shell by absolute path (/usr/bin/fish) or by common name (bash, zsh, fish, sh, dash, ksh). If omitted, the system default shell is used.
- Adjustable output capture: raise output_limit (bytes) for verbose commands. Large output may still be truncated; the response clarifies when truncation occurred (and detached jobs expose a preview via status).
- Timeout-based detachment: set timeout_seconds > 0 on a normal (non-detached) run to wait up to that many seconds; if the command is still running, the tool returns {"job_id":"…","state":"running"} and continues execution in the background.
- Clear result reporting: exit code, truncated output notice, and success/failure classification are included in the response.
- Background jobs: detach execution, poll status, optionally cancel (best-effort with OS signals on Unix), request full untruncated output once finished, and list currently known jobs.

Input fields:
- command (string, required unless performing a status / control query): The shell snippet to execute. Do not include cd; instead use cwd.
- cwd (string, optional, default "."):
  * "." or "" – resolve to the single worktree root (fails if multiple roots exist).
  * Absolute path – must exist (anywhere on the filesystem).
  * Worktree name – resolves to that project root.
- use_sudo (bool, optional, default false): Whether to run with elevated privileges (via sudo -S).
- shell (string, optional): Preferred shell name or full path. Falls back to best match or the system default.
- output_limit (integer, optional): Maximum bytes of combined stdout/stderr to retain in immediate or detached preview (defaults: 16KB normal, 256KB when sudo requested unless overridden).
- timeout_seconds (integer, optional, default 0): When > 0 in a non-detached run, wait up to this many seconds; if still running, return a job_id and keep the process running in the background (no hard kill).
- detach (bool, optional, default false): If true, run the command in the background and return immediately with {"job_id":"…","state":"running"}.
- job_id (string, optional): When provided with an empty command, returns status for a previously detached job.
- full_output (bool, optional, status mode only): When true and the job is finished or canceled, returns the full untruncated output instead of a preview snippet.
- cancel (bool, optional, status mode only): Attempts graceful termination (SIGTERM then SIGKILL after ~5s on Unix); on non‑Unix, marks the job canceled as best-effort.
- list_jobs (bool, optional, status mode only): When true with an empty command, returns a JSON list of all known jobs and their current states, runtime, and output preview.

Behavior details:
- Each invocation is isolated; no state is preserved between runs (except detached job bookkeeping).
- Output ordering preserves interleaving of stdout and stderr as produced.
- If truncated, only the leading portion (up to the effective limit) is kept in the preview; full_output can later retrieve the entire captured buffer.
- Authorization step occurs before executing (especially important for sudo).
- On non‑zero exit codes, the command string and exit status are reported; captured output (if any) is still returned.
- If the process is terminated, canceled, or no clean exit status is available, the tool reports interruption or cancellation and includes any partial output.
- Detached jobs store both a truncated preview (output_limit) and the full captured output for subsequent retrieval.

When to choose this tool instead of the basic terminal:
Use enhanced_terminal when you need one or more of:
- Sudo privileges
- A non-project directory
- A specific shell different from the default
- Larger or tunable output capture
- A timeout that returns a job_id if exceeded
- Detached execution, status polling, cancellation, full output retrieval, or job listing

Use the basic terminal tool when:
- You only need a simple one-liner inside a project root
- Standard output limits (16KB) are sufficient
- No elevation, custom shell, timeout-based detachment, or background job features are required

Detached execution / status / cancellation / full output / streaming preview:
To run detached:
  command: "sleep 30 && echo done"
  detach: true

To run synchronously but detach if it exceeds 5 seconds:
  command: "long_task.sh"
  timeout_seconds: 5

To poll status before completion:
  command: ""
  job_id: "enhterm-job-1"

To request cancellation (signal-based on Unix):
  command: ""
  job_id: "enhterm-job-1"
  cancel: true

To retrieve the full output (after job finished or canceled):
  command: ""
  job_id: "enhterm-job-1"
  full_output: true

To list all known jobs:
  command: ""
  list_jobs: true

Status states:
- running
- finished
- canceled
- not_found

Returned preview JSON example:
  {"job_id":"enhterm-job-1","state":"running","exit_code":null,"success":false,"truncated":false,"canceled":false,"runtime_secs":12,"preview":""}

After completion with full_output request:
  {"job_id":"enhterm-job-1","state":"finished","exit_code":0,"success":true,"truncated":false,"canceled":false,"runtime_secs":31,"full_output":"...full text..."}

Best practices:
- Prefer read-only commands unless changes are explicitly requested.
- Validate paths before using sudo to avoid accidental destructive operations.
- Keep commands minimal; chain only what is necessary for clarity and safety.
- For large data inspections (logs, binaries), consider pagination tools (head, tail, grep) to reduce output size.
- Use timeout_seconds to avoid blocking indefinitely; rely on job_id for long operations.
- Use detach for long-running operations instead of increasing timeouts arbitrarily.
- Use preview first; only request full_output when necessary to reduce transport payload size.

Examples (conceptual, not literal JSON):

1. List system log directory with full path:
  command: "ls -1"
  cwd: "/var/log"

2. Use fish shell for a multi-line script:
  command: "for f in *.rs; echo $f; end"
  shell: "fish"

3. Elevated package index refresh (needs justification):
  command: "apt-get update"
  use_sudo: true
  timeout_seconds: 120
  output_limit: 262144

4. Inspect a large log with truncation control:
  command: "grep ERROR app.log | head -n 200"
  cwd: "backend"
  output_limit: 65536

5. Detached long-running build:
  command: "cargo build --workspace"
  detach: true
  output_limit: 65536

6. Poll and get full output after finish:
  command: ""
  job_id: "enhterm-job-5"
  full_output: true

7. Enumerate current jobs:
  command: ""
  list_jobs: true

Streaming behavior:
- While a detached job runs, output is read incrementally and a truncated preview (up to output_limit) is updated in memory.
- Status polls return the latest preview (preview does not require re-running the command).
- full_output=true (after finish or cancellation) returns the entire captured buffer (may be large).

Real cancellation:
- cancel:true on a running detached job attempts a graceful termination:
  * Unix: sends SIGTERM, then escalates to SIGKILL after ~5s if the process still exists.
  * Non-Unix: marks the job canceled (best-effort; underlying process termination may be limited by platform constraints).
- Canceled jobs report state:"canceled" and can still return full_output.

Safety denylist:
- Destructive / risky patterns are blocked by default. To run a matching command you must set allow_dangerous:true in the tool input AND have the global setting agent.enhanced_terminal_allow_dangerous=true.
- Built-in dangerous patterns (substring match, case-insensitive):
  rm -rf /
  mkfs
  :(){:|:&};:
  dd if=
  shutdown -h
  reboot
  chmod 777 /
  chown root:
- Settings (configured in settings.json / default.json under agent.*):
  * agent.enhanced_terminal_allow_dangerous (bool, default false): Must be true (along with per-call allow_dangerous:true) to permit execution of a matching dangerous/denylisted command.
  * agent.enhanced_terminal_denylist (array of strings, default []): Additional case-insensitive substring patterns appended to the built-ins.
  * agent.enhanced_terminal_disable_default_denylist (bool, default false): When true, removes the built-in patterns so only the custom denylist applies.
- Rationale: Centralized, auditable safety configuration via settings.

Overriding safety:
- Set allow_dangerous:true only when you intentionally need a blocked pattern.
- Always justify the necessity of dangerous commands in user-visible reasoning.

Security & safety reminders:
- Always justify sudo usage to the user during authorization.
- Avoid commands that daemonize or run indefinitely unless a timeout or detach mode is used intentionally.
- Do not embed secrets directly in the command string.
- Prefer absolute paths when operating outside project roots to avoid ambiguity.
- Consider using preview first, then full_output for large logs to reduce exposure and bandwidth.

Returned response (non-detached) will summarize:
- Success / failure / timeout / interruption
- Exit code (when available)
- Truncation notice (if output exceeded limit)
- Captured (possibly truncated) output wrapped in a fenced code block

Detached lifecycle:
- Initial detach or timeout-based detachment: {"job_id":"...","state":"running"}
- Status poll: {"job_id":"...","state":"running|finished|canceled","exit_code":<int|null>,"success":<bool>,"truncated":<bool>,"canceled":<bool>,"runtime_secs":<int>,"preview":"<first bytes>"}
- Full output retrieval (finished/canceled + full_output=true): same plus "full_output"
- Job listing: {"jobs":[{"job_id":"…","state":"…","exit_code":<int|null>,"success":<bool>,"truncated":<bool>,"canceled":<bool>,"runtime_secs":<int>,"preview":"…","command":"…"}, ...]}