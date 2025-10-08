use agent_client_protocol as acp;
use anyhow::Result;
use futures::FutureExt;
use gpui::{App, Entity, SharedString, Task};
use project::Project;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::Duration,
};
use util::markdown::MarkdownInlineCode;

use crate::{AgentTool, ThreadEnvironment, ToolCallEventStream};

const DEFAULT_OUTPUT_LIMIT: u64 = 16 * 1024;
const LARGE_OUTPUT_LIMIT: u64 = 256 * 1024;

/// Executes a shell command with enhanced capabilities including sudo support.
///
/// This tool provides advanced terminal execution features:
/// - Execute commands with sudo privileges (requires authorization)
/// - Run commands in any directory on the system (not limited to project directories)
/// - Choose specific shells for execution
/// - Support for long-running commands with configurable output limits
/// - Async execution for commands that take time
///
/// The output results will be shown to the user already, only list it again if necessary, avoid being redundant.
///
/// When using sudo, ensure the command is necessary and explain why to the user during authorization.
///
/// Remember that each invocation spawns a new shell process, so you can't rely on state from previous invocations.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct EnhancedTerminalToolInput {
    /// The command to execute. Can be a one-liner or multi-line script.
    command: String,

    /// Working directory for the command. Can be any absolute path on the system, or a relative path within the project.
    /// Use "." for the project root (only valid for single-root workspaces).
    #[serde(default = "default_working_dir")]
    cwd: String,

    /// Whether to execute the command with sudo privileges.
    /// This requires explicit user authorization and should only be used when necessary.
    #[serde(default)]
    use_sudo: bool,

    /// Shell to use for execution. If not specified, uses the system default shell.
    /// Examples: "/bin/bash", "/bin/zsh", "/usr/bin/fish", "bash", "zsh", "fish"
    #[serde(default)]
    shell: Option<String>,

    /// Maximum output size in bytes. Defaults to 16KB for normal commands, but can be increased for long-running commands.
    /// Use larger values (e.g., 262144 for 256KB) for commands with extensive output.
    #[serde(default)]
    output_limit: Option<u64>,

    /// Optional timeout in seconds. If not specified, the command can run indefinitely.
    /// Use with caution for long-running processes.
    #[serde(default)]
    timeout_seconds: Option<u64>,
}

fn default_working_dir() -> String {
    ".".to_string()
}

pub struct EnhancedTerminalTool {
    project: Entity<Project>,
    environment: Rc<dyn ThreadEnvironment>,
}

impl EnhancedTerminalTool {
    pub fn new(project: Entity<Project>, environment: Rc<dyn ThreadEnvironment>) -> Self {
        Self {
            project,
            environment,
        }
    }
}

impl AgentTool for EnhancedTerminalTool {
    type Input = EnhancedTerminalToolInput;
    type Output = String;

    fn name() -> &'static str {
        "enhanced_terminal"
    }

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Execute
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        if let Ok(input) = input {
            let prefix = if input.use_sudo { "sudo " } else { "" };
            let mut lines = input.command.lines();
            let first_line = lines.next().unwrap_or_default();
            let remaining_line_count = lines.count();

            match remaining_line_count {
                0 => MarkdownInlineCode(&format!("{}{}", prefix, first_line))
                    .to_string()
                    .into(),
                1 => MarkdownInlineCode(&format!(
                    "{}{} - {} more line",
                    prefix, first_line, remaining_line_count
                ))
                .to_string()
                .into(),
                n => MarkdownInlineCode(&format!("{}{} - {} more lines", prefix, first_line, n))
                    .to_string()
                    .into(),
            }
        } else {
            "".into()
        }
    }

    fn run(
        self: Arc<Self>,
        input: Self::Input,
        event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output>> {
        let working_dir = match resolve_working_directory(&input, &self.project, cx) {
            Ok(dir) => dir,
            Err(err) => return Task::ready(Err(err)),
        };

        let final_command = prepare_command(&input);
        let output_limit = input.output_limit.unwrap_or(if input.use_sudo {
            LARGE_OUTPUT_LIMIT
        } else {
            DEFAULT_OUTPUT_LIMIT
        });

        let authorize = event_stream.authorize(self.initial_title(Ok(input.clone()), cx), cx);

        cx.spawn(async move |cx| {
            authorize.await?;

            let terminal = self
                .environment
                .create_terminal(final_command.clone(), working_dir, Some(output_limit), cx)
                .await?;

            let terminal_id = terminal.id(cx)?;
            event_stream.update_fields(acp::ToolCallUpdateFields {
                content: Some(vec![acp::ToolCallContent::Terminal { terminal_id }]),
                ..Default::default()
            });

            let exit_status = if let Some(timeout) = input.timeout_seconds {
                wait_with_timeout(terminal.wait_for_exit(cx)?, timeout).await?
            } else {
                terminal.wait_for_exit(cx)?.await
            };

            let output = terminal.current_output(cx)?;

            Ok(process_command_output(
                output,
                &input.command,
                exit_status,
                input.use_sudo,
            ))
        })
    }
}

fn resolve_working_directory(
    input: &EnhancedTerminalToolInput,
    project: &Entity<Project>,
    cx: &mut App,
) -> Result<Option<PathBuf>> {
    let cd = &input.cwd;
    let project_ref = project.read(cx);

    if cd == "." || cd.is_empty() {
        let mut worktrees = project_ref.worktrees(cx);
        match worktrees.next() {
            Some(worktree) => {
                anyhow::ensure!(
                    worktrees.next().is_none(),
                    "'.' is ambiguous in multi-root workspaces. Please specify a root directory explicitly.",
                );
                Ok(Some(worktree.read(cx).abs_path().to_path_buf()))
            }
            None => Ok(None),
        }
    } else {
        let input_path = Path::new(cd);

        if input_path.is_absolute() {
            if input_path.exists() {
                Ok(Some(input_path.to_path_buf()))
            } else {
                anyhow::bail!(
                    "Directory '{}' does not exist. Please specify a valid absolute path.",
                    cd
                );
            }
        } else if let Some(worktree) = project_ref.worktree_for_root_name(cd, cx) {
            Ok(Some(worktree.read(cx).abs_path().to_path_buf()))
        } else {
            anyhow::bail!(
                "Directory '{}' was not found. Specify an absolute path (e.g., '/home/user/project') or a project worktree name.",
                cd
            );
        }
    }
}

fn prepare_command(input: &EnhancedTerminalToolInput) -> String {
    let mut command = input.command.clone();

    if input.use_sudo {
        command = format!("sudo -S {}", command);
    }

    if let Some(shell) = &input.shell {
        let shell_path = resolve_shell_path(shell);
        command = format!("{} -c '{}'", shell_path, command.replace("'", "'\\''"));
    }

    command
}

fn resolve_shell_path(shell: &str) -> String {
    if shell.starts_with('/') && Path::new(shell).exists() {
        return shell.to_string();
    }

    let common_paths = match shell {
        "bash" => vec!["/bin/bash", "/usr/bin/bash"],
        "zsh" => vec!["/bin/zsh", "/usr/bin/zsh", "/usr/local/bin/zsh"],
        "fish" => vec!["/bin/fish", "/usr/bin/fish", "/usr/local/bin/fish"],
        "sh" => vec!["/bin/sh", "/usr/bin/sh"],
        "dash" => vec!["/bin/dash", "/usr/bin/dash"],
        "ksh" => vec!["/bin/ksh", "/usr/bin/ksh"],
        _ => vec![],
    };

    for path in common_paths {
        if Path::new(path).exists() {
            return path.to_string();
        }
    }

    shell.to_string()
}

async fn wait_with_timeout(
    wait_task: futures::future::Shared<Task<acp::TerminalExitStatus>>,
    timeout_seconds: u64,
) -> Result<acp::TerminalExitStatus> {
    let timeout = smol::Timer::after(Duration::from_secs(timeout_seconds));

    futures::select! {
        result = wait_task.fuse() => Ok(result),
        _ = timeout.fuse() => {
            anyhow::bail!("Command execution timed out after {} seconds", timeout_seconds)
        }
    }
}

fn process_command_output(
    output: acp::TerminalOutputResponse,
    command: &str,
    exit_status: acp::TerminalExitStatus,
    used_sudo: bool,
) -> String {
    let content = output.output.trim();
    let is_empty = content.is_empty();

    let content = format!("```\n{content}\n```");
    let content = if output.truncated {
        format!(
            "Command output too long. The first {} bytes:\n\n{content}",
            content.len(),
        )
    } else {
        content
    };

    let sudo_prefix = if used_sudo { "[SUDO] " } else { "" };

    let content = match exit_status.exit_code {
        Some(0) => {
            if is_empty {
                format!("{}Command executed successfully.", sudo_prefix)
            } else {
                format!(
                    "{}Command executed successfully.\n\n{}",
                    sudo_prefix, content
                )
            }
        }
        Some(exit_code) => {
            if is_empty {
                format!(
                    "{}Command \"{}\" failed with exit code {}.",
                    sudo_prefix, command, exit_code
                )
            } else {
                format!(
                    "{}Command \"{}\" failed with exit code {}.\n\n{}",
                    sudo_prefix, command, exit_code, content
                )
            }
        }
        None => {
            format!(
                "{}Command failed or was interrupted.\nPartial output captured:\n\n{}",
                sudo_prefix, content,
            )
        }
    };

    content
}
