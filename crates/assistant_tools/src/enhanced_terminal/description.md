Execute shell commands with advanced capabilities including optional sudo elevation, flexible working directory selection (inside or outside project roots), custom shell choice, larger output limits, and optional timeouts.



Key features:
- Run commands in any directory: pass an absolute path (e.g. /var/log) or a project worktree name, or use "." when there is exactly one worktree.
- Optional sudo: set use_sudo: true to prefix the command with sudo -S (authorization required). Only use when strictly necessary and prefer read‑only or least‑privilege operations.
- Custom shell: choose a shell by absolute path (/usr/bin/fish) or by common name (bash, zsh, fish, sh, dash, ksh). If omitted, the system default shell is used.
- Adjustable output capture: raise output_limit (bytes) for verbose commands. Large output may still be truncated; the response clarifies when truncation occurred.
- Long running commands: omit timeout_seconds for no explicit timeout, or set a positive value to fail fast if the process exceeds that duration.
- Clear result reporting: exit code, truncated output notice, and success/failure classification are included in the response.

Input fields:
- command (string, required): The exact shell snippet to execute. Do not include cd; instead use cwd.
- cwd (string, optional, default "."):
  * "." or "" – resolve to the single worktree root (fails if multiple roots exist).
  * Absolute path – must exist (anywhere on the filesystem).
  * Worktree name – resolves to that project root.
- use_sudo (bool, optional, default false): Whether to run with elevated privileges (via sudo -S).
- shell (string, optional): Preferred shell name or full path. Falls back to best match or the system default.
- output_limit (integer, optional): Maximum bytes of combined stdout/stderr to retain (defaults: 16KB normal, 256KB when sudo requested unless overridden).
- timeout_seconds (integer, optional): Hard limit after which the command is aborted with a timeout error.

Behavior details:
- Each invocation is isolated; no state is preserved between runs.
- Output ordering preserves interleaving of stdout and stderr as produced.
- If truncated, only the leading portion (up to the effective limit) is kept.
- Authorization step occurs before executing (especially important for sudo).
- On non‑zero exit codes, the command string and exit status are reported; captured output (if any) is still returned.
- If the process is terminated or no clean exit status is available, the tool reports that it was interrupted and includes any partial output.

When to choose this tool instead of the basic terminal:
Use enhanced_terminal when you need one or more of:
- Sudo privileges
- A non-project directory
- A specific shell different from the default
- Larger or tunable output capture
- A configurable timeout

Use the basic terminal tool when:
- You only need a simple one-liner inside a project root
- Standard output limits (16KB) are sufficient
- No elevation, custom shell, or timeout is required

Best practices:
- Prefer read-only commands unless changes are explicitly requested.
- Validate paths before using sudo to avoid accidental destructive operations.
- Keep commands minimal; chain only what is necessary for clarity and safety.
- For large data inspections (logs, binaries), consider pagination tools (head, tail, grep) to reduce output size.
- Explicitly set a timeout for commands that could hang (e.g. network diagnostics) to avoid indefinite execution.

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

Security & safety reminders:
- Always justify sudo usage to the user during authorization.
- Avoid commands that daemonize or run indefinitely unless a timeout is set.
- Do not embed secrets directly in the command string.
- Prefer absolute paths when operating outside project roots to avoid ambiguity.

Returned response will summarize:
- Success / failure / timeout / interruption
- Exit code (when available)
- Truncation notice (if output exceeded limit)
- Captured (possibly truncated) output wrapped in a fenced code block