# Enhanced Terminal Tools

This document describes the enhanced terminal capabilities added to the agent system.

## Overview

Two new tools have been added to provide advanced terminal execution capabilities:

1. **`enhanced_terminal`** - Execute commands with sudo, flexible directory handling, shell selection, and long-running command support
2. **`detect_shells`** - Detect and list available shells on the system

## Enhanced Terminal Tool

### Features

- **Sudo Support**: Execute commands with elevated privileges (requires explicit user authorization)
- **Flexible Directory Handling**: Run commands in any directory on the system, not limited to project directories
- **Shell Selection**: Choose specific shells for execution (bash, zsh, fish, etc.)
- **Configurable Output Limits**: Support for long-running commands with adjustable output size limits
- **Timeout Support**: Optional timeout for commands to prevent hanging
- **Async Execution**: Proper async handling for time-consuming operations

### Usage

The `enhanced_terminal` tool accepts the following parameters:

```json
{
  "command": "ls -la",
  "cwd": "/home/user/projects",
  "use_sudo": false,
  "shell": "bash",
  "output_limit": 262144,
  "timeout_seconds": 300
}
```

#### Parameters

- **`command`** (required): The command to execute. Can be a one-liner or multi-line script.

- **`cwd`** (optional, default: "."): Working directory for the command.
  - Use "." for the project root (only valid for single-root workspaces)
  - Can be any absolute path on the system (e.g., "/etc", "/home/user")
  - Can be a project worktree name

- **`use_sudo`** (optional, default: false): Whether to execute with sudo privileges.
  - Requires explicit user authorization
  - Should only be used when necessary
  - Automatically increases output limit to 256KB

- **`shell`** (optional): Shell to use for execution.
  - Can be a full path: "/bin/bash", "/usr/bin/zsh"
  - Can be a shell name: "bash", "zsh", "fish"
  - If not specified, uses the system default shell
  - The tool will attempt to resolve common shell locations

- **`output_limit`** (optional): Maximum output size in bytes.
  - Default: 16KB for normal commands
  - Default: 256KB for sudo commands
  - Can be increased for commands with extensive output

- **`timeout_seconds`** (optional): Optional timeout in seconds.
  - If not specified, command can run indefinitely
  - Use with caution for long-running processes
  - Command will be terminated if timeout is exceeded

### Examples

#### Basic command in project directory:
```json
{
  "command": "npm install",
  "cwd": "."
}
```

#### Command with sudo in system directory:
```json
{
  "command": "apt-get update",
  "cwd": "/",
  "use_sudo": true
}
```

#### Command with specific shell:
```json
{
  "command": "echo $SHELL",
  "shell": "zsh",
  "cwd": "/home/user"
}
```

#### Long-running command with timeout:
```json
{
  "command": "cargo build --release",
  "cwd": "/home/user/rust-project",
  "output_limit": 524288,
  "timeout_seconds": 600
}
```

## Shell Detector Tool

### Features

- Scans common shell locations on the system
- Reports available shells with their paths
- Shows the current shell from $SHELL environment variable
- Helps determine which shells can be used with `enhanced_terminal`

### Usage

The `detect_shells` tool takes no parameters:

```json
{}
```

### Example Output

```markdown
Available shells:

- **bash**: `/bin/bash`
- **zsh**: `/usr/bin/zsh`
- **fish**: `/usr/bin/fish`
- **sh**: `/bin/sh`

Current shell (from $SHELL): `/usr/bin/fish`

You can use any of these shells when executing terminal commands by specifying the full path or shell name.
```

## Security Considerations

### Sudo Execution

- Commands with `use_sudo: true` require explicit user authorization
- Users will be prompted to approve sudo commands before execution
- Explain to users why sudo is necessary during authorization
- Avoid using sudo unless absolutely required

### Directory Access

- The tool can access any directory on the system
- Ensure commands are executed in appropriate directories
- Be cautious when operating in system directories (/, /etc, /usr, etc.)
- Validate paths before executing commands

### Command Safety

- Avoid running commands that could harm the system
- Be careful with destructive commands (rm, dd, etc.)
- Consider the implications of long-running commands
- Use timeouts for commands that might hang

## Implementation Details

### Architecture

- **`enhanced_terminal_tool.rs`**: Main tool implementation
- **`shell_detector_tool.rs`**: Shell detection implementation
- Both tools are registered in `thread.rs` alongside existing tools

### Key Functions

- `resolve_working_directory()`: Validates and resolves the working directory
- `prepare_command()`: Prepares the command with sudo and shell wrapping
- `resolve_shell_path()`: Resolves shell names to full paths
- `wait_with_timeout()`: Implements timeout functionality
- `detect_available_shells()`: Scans for available shells

### Integration

The tools integrate with the existing agent infrastructure:

1. Registered in `add_default_tools()` in `thread.rs`
2. Exported from `tools.rs` module
3. Use the standard `AgentTool` trait
4. Support authorization through `ToolCallEventStream`
5. Report terminal output through the agent client protocol

## Migration from Standard Terminal Tool

The original `terminal` tool remains available for backward compatibility. The `enhanced_terminal` tool provides a superset of functionality:

### When to use `terminal`:
- Simple commands in project directories
- No need for sudo or special shells
- Standard output limits are sufficient

### When to use `enhanced_terminal`:
- Need sudo privileges
- Operating outside project directories
- Require specific shell selection
- Long-running commands with large output
- Need timeout control

## Future Enhancements

Potential improvements for future versions:

- Interactive command support (stdin input)
- Environment variable configuration
- Command history and caching
- Background process management
- Stream output in real-time
- Multi-command pipelines
- Command templates