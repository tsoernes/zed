# Tools

Zed's Agent has access to a variety of tools that allow it to interact with your codebase and perform tasks.

## Read & Search Tools

### `diagnostics`

Gets errors and warnings for either a specific file or the entire project, useful after making edits to determine if further changes are needed.

### `fetch`

Fetches a URL and returns the content as Markdown. Useful for providing docs as context.

### `find_path`

Quickly finds files by matching glob patterns (like "\*_/_.js"), returning matching file paths alphabetically.

### `grep`

Searches file contents across the project using regular expressions, preferred for finding symbols in code without knowing exact file paths.

### `list_directory`

Lists files and directories in a given path, providing an overview of filesystem contents.

### `now`

Returns the current date and time.

### `open`

Opens a file or URL with the default application associated with it on the user's operating system.

### `read_file`

Reads the content of a specified file in the project, allowing access to file contents.

### `thinking`

Allows the Agent to work through problems, brainstorm ideas, or plan without executing actions, useful for complex problem-solving.

### `web_search`

Searches the web for information, providing results with snippets and links from relevant web pages, useful for accessing real-time information.

## Edit Tools

### `copy_path`

Copies a file or directory recursively in the project, more efficient than manually reading and writing files when duplicating content.

### `create_directory`

Creates a new directory at the specified path within the project, creating all necessary parent directories (similar to `mkdir -p`).

### `create_file`

Creates a new file at a specified path with given text content, the most efficient way to create new files or completely replace existing ones.

### `delete_path`

Deletes a file or directory (including contents recursively) at the specified path and confirms the deletion.

### `edit_file`

Edits files by replacing specific text with new content.

### `move_path`

Moves or renames a file or directory in the project, performing a rename if only the filename differs.

### `terminal`

Executes shell commands and returns the combined output, creating a new shell process for each invocation.

For multitasking and parallel execution, prefer the async terminal tool `enhanced_terminal_async`. It starts the process in the background and returns a `job_id` immediately (or after waiting up to `timeout_seconds` if provided). Use `enhanced_terminal_job_status` to poll state, optionally cancel or fetch full output, and `enhanced_terminal_list_jobs` to enumerate all jobs.

Examples:

- Start async and return immediately with a job ID:
  - Tool: `enhanced_terminal_async`
  - Input: `{ "command": "sleep 30 && echo done", "cwd": ".", "timeout_seconds": 0 }`

- Start async and wait up to 5 seconds for completion before returning a job ID:
  - Tool: `enhanced_terminal_async`
  - Input: `{ "command": "long_task.sh", "timeout_seconds": 5 }`

- Poll status and fetch full output when finished:
  - Tool: `enhanced_terminal_job_status`
  - Input: `{ "job_id": "enhterm-job-1", "full_output": true }`

- Cancel a running job (best-effort, signal-based on Unix):
  - Tool: `enhanced_terminal_job_status`
  - Input: `{ "job_id": "enhterm-job-1", "cancel": true }`

- List all known jobs:
  - Tool: `enhanced_terminal_list_jobs`
  - Input: `{}`

