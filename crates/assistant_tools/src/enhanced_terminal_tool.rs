use crate::schema::json_schema_for;
use action_log::ActionLog;
use anyhow::{Result, anyhow};
use assistant_tool::{Tool, ToolResult};
use gpui::AppContext;
use gpui::{AnyWindowHandle, App, Entity, Task};
use language_model::{LanguageModel, LanguageModelRequest, LanguageModelToolSchemaFormat};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use project::Project;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{
    env,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};
use ui::IconName;
use util::markdown::MarkdownInlineCode;

const DEFAULT_OUTPUT_LIMIT: usize = 16 * 1024;
const LARGE_OUTPUT_LIMIT: usize = 256 * 1024;

/// Input for the minimal enhanced terminal tool.
/// This headless-only implementation:
/// - Supports optional sudo
/// - Allows any absolute path as cwd (even outside project)
/// - Allows specifying a shell
/// - Allows adjusting output limit
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct EnhancedTerminalToolInput {
    /// Command to execute (one-liner or multi-line; no embedded cd).
    command: String,
    /// Working directory: ".", empty, absolute path, or project worktree name.
    #[serde(default = "default_cwd")]
    cwd: String,
    /// Run with sudo -S (authorization still required by the host).
    #[serde(default)]
    use_sudo: bool,
    /// Shell name or absolute path (e.g. "bash", "fish", "/usr/bin/fish").
    #[serde(default)]
    shell: Option<String>,
    /// Max captured bytes (defaults: 16KB normal, 256KB with sudo unless overridden).
    #[serde(default)]
    output_limit: Option<usize>,
}

fn default_cwd() -> String {
    ".".into()
}

pub struct EnhancedTerminalTool;

impl EnhancedTerminalTool {
    pub const NAME: &str = "enhanced_terminal";
}

impl Tool for EnhancedTerminalTool {
    fn name(&self) -> String {
        Self::NAME.to_string()
    }

    fn needs_confirmation(
        &self,
        _input: &serde_json::Value,
        _project: &Entity<Project>,
        _cx: &App,
    ) -> bool {
        true
    }

    fn may_perform_edits(&self) -> bool {
        false
    }

    fn description(&self) -> String {
        // Description sourced from ./enhanced_terminal/description.md (directory renamed from enhanced_terminal_tool)
        include_str!("./enhanced_terminal/description.md").to_string()
    }

    fn icon(&self) -> IconName {
        IconName::ToolTerminal
    }

    fn input_schema(&self, format: LanguageModelToolSchemaFormat) -> Result<serde_json::Value> {
        json_schema_for::<EnhancedTerminalToolInput>(format)
    }

    fn ui_text(&self, input: &serde_json::Value) -> String {
        match serde_json::from_value::<EnhancedTerminalToolInput>(input.clone()) {
            Ok(input) => {
                let mut lines = input.command.lines();
                let first = lines.next().unwrap_or_default();
                let rest = lines.count();
                let prefix = if input.use_sudo { "sudo " } else { "" };
                match rest {
                    0 => MarkdownInlineCode(&format!("{prefix}{first}")).to_string(),
                    1 => MarkdownInlineCode(&format!("{prefix}{first} - 1 more line")).to_string(),
                    n => {
                        MarkdownInlineCode(&format!("{prefix}{first} - {n} more lines")).to_string()
                    }
                }
            }
            Err(_) => "Run enhanced terminal command".into(),
        }
    }

    fn run(
        self: Arc<Self>,
        input: serde_json::Value,
        _request: Arc<LanguageModelRequest>,
        project: Entity<Project>,
        _action_log: Entity<ActionLog>,
        _model: Arc<dyn LanguageModel>,
        _window: Option<AnyWindowHandle>,
        cx: &mut App,
    ) -> ToolResult {
        let input: EnhancedTerminalToolInput = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return Task::ready(Err(anyhow!(e))).into(),
        };

        let working_dir = match resolve_working_directory(&input, &project, cx) {
            Ok(dir) => dir,
            Err(err) => return Task::ready(Err(err)).into(),
        };

        let effective_limit = input.output_limit.unwrap_or(if input.use_sudo {
            LARGE_OUTPUT_LIMIT
        } else {
            DEFAULT_OUTPUT_LIMIT
        });

        let final_script = build_final_command(&input);

        // Headless execution only: always use a pty and collect output.
        let task = cx.background_spawn(async move {
            let pty_system = native_pty_system();
            let (program, args) = shell_command_parts(&final_script, &input.shell);
            let mut cmd = CommandBuilder::new(program);
            cmd.args(args);

            if let Some(cwd) = &working_dir {
                cmd.cwd(cwd);
            }

            // Provide minimal environment override similar to existing terminal tool.
            if cfg!(unix) {
                cmd.env("PAGER", "cat");
            }
            if let Ok(home) = env::var("HOME") {
                cmd.env("HOME", home);
            }

            let pair = pty_system.openpty(PtySize {
                rows: 24,
                cols: 80,
                ..Default::default()
            })?;

            let mut child = pair.slave.spawn_command(cmd)?;
            let mut reader = pair.master.try_clone_reader()?;
            drop(pair);

            let mut raw = String::new();
            reader.read_to_string(&mut raw)?;
            let status = child.wait()?;

            // portable_pty::ExitStatus -> attempt success() and (if available) exit_code() access
            let (exit_code, success) = {
                #[cfg(unix)]
                {
                    (Some(status.exit_code() as i32), status.success())
                }
                #[cfg(not(unix))]
                {
                    (None, status.success())
                }
            };

            let output = process_output(
                &raw,
                &input.command,
                exit_code,
                success,
                input.use_sudo,
                effective_limit,
            );
            Ok(output.into())
        });

        ToolResult {
            output: task,
            card: None,
        }
    }
}

fn resolve_working_directory(
    input: &EnhancedTerminalToolInput,
    project: &Entity<Project>,
    cx: &mut App,
) -> Result<Option<PathBuf>> {
    let cd = input.cwd.trim();
    if cd.is_empty() || cd == "." {
        // Single-worktree resolution (same logic pattern as basic terminal tool)
        let mut iter = project.read(cx).worktrees(cx);
        match iter.next() {
            Some(worktree) => {
                if iter.next().is_some() {
                    return Err(anyhow!(
                        "'.' is ambiguous in a multi-root workspace. Specify a worktree name or absolute path."
                    ));
                }
                return Ok(Some(worktree.read(cx).abs_path().to_path_buf()));
            }
            None => return Ok(None),
        }
    }

    let path = Path::new(cd);
    if path.is_absolute() {
        if path.exists() {
            return Ok(Some(path.to_path_buf()));
        } else {
            return Err(anyhow!("Directory '{cd}' does not exist."));
        }
    }

    // Try resolve as worktree name
    if let Some(wt) = project.read(cx).worktree_for_root_name(cd, cx) {
        return Ok(Some(wt.read(cx).abs_path().to_path_buf()));
    }

    Err(anyhow!(
        "Directory '{cd}' not found. Use '.', a worktree name, or an absolute existing path."
    ))
}

fn build_final_command(input: &EnhancedTerminalToolInput) -> String {
    let mut cmd = input.command.clone();
    if input.use_sudo {
        cmd = format!("sudo -S {cmd}");
    }
    cmd
}

fn shell_command_parts(final_script: &str, shell: &Option<String>) -> (String, Vec<String>) {
    if let Some(spec) = shell {
        let resolved = resolve_shell_path(spec);
        (
            resolved,
            vec![
                "-c".into(),
                // naive single-quote escaping for embedding inside a -c '...'
                final_script.replace('\'', "'\\''"),
            ],
        )
    } else {
        // Use default system shell provided by util helper (same as basic terminal)
        let default = util::get_default_system_shell();
        (
            default.clone(),
            vec!["-c".into(), final_script.replace('\'', "'\\''")],
        )
    }
}

fn resolve_shell_path(spec: &str) -> String {
    if spec.starts_with('/') {
        return spec.to_string();
    }
    let candidates = match spec {
        "bash" => &["/bin/bash", "/usr/bin/bash"][..],
        "zsh" => &["/bin/zsh", "/usr/bin/zsh", "/usr/local/bin/zsh"][..],
        "fish" => &["/bin/fish", "/usr/bin/fish", "/usr/local/bin/fish"][..],
        "sh" => &["/bin/sh", "/usr/bin/sh"][..],
        "dash" => &["/bin/dash", "/usr/bin/dash"][..],
        "ksh" => &["/bin/ksh", "/usr/bin/ksh"][..],
        _ => &[][..],
    };
    for c in candidates {
        if Path::new(c).exists() {
            return c.to_string();
        }
    }
    // Fallback: rely on shell being in PATH
    spec.to_string()
}

fn process_output(
    raw: &str,
    original_command: &str,
    exit_code: Option<i32>,
    success: bool,
    used_sudo: bool,
    limit: usize,
) -> String {
    let mut content = raw.trim().to_string();

    let truncated = if content.len() > limit {
        let mut end_ix = limit;
        while !content.is_char_boundary(end_ix) && end_ix > 0 {
            end_ix -= 1;
        }
        content.truncate(end_ix);
        true
    } else {
        false
    };

    let empty = content.is_empty();
    let fenced = format!("```\n{}\n```", content);
    let fenced = if truncated {
        format!(
            "Command output too long. The first {} bytes:\n\n{}",
            content.len(),
            fenced
        )
    } else {
        fenced
    };
    let sudo_prefix = if used_sudo { "[SUDO] " } else { "" };

    match exit_code {
        Some(code) if code == 0 && success => {
            if empty {
                format!("{sudo_prefix}Command executed successfully.")
            } else {
                format!("{sudo_prefix}Command executed successfully.\n\n{fenced}")
            }
        }
        Some(code) => {
            if empty {
                format!(
                    "{sudo_prefix}Command \"{}\" failed with exit code {}.",
                    original_command, code
                )
            } else {
                format!(
                    "{sudo_prefix}Command \"{}\" failed with exit code {}.\n\n{}",
                    original_command, code, fenced
                )
            }
        }
        None => {
            // No exit code (platform or interruption)
            if empty {
                format!("{sudo_prefix}Command failed or was interrupted (no output).")
            } else {
                format!(
                    "{sudo_prefix}Command failed or was interrupted.\nPartial output captured:\n\n{}",
                    fenced
                )
            }
        }
    }
}

// Only the canonical "enhanced_terminal" identifier is supported.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema_builds() {
        let _ = json_schema_for::<EnhancedTerminalToolInput>(
            language_model::LanguageModelToolSchemaFormat::JsonSchema,
        )
        .unwrap();
    }

    #[test]
    fn test_process_output_success() {
        let out = process_output("hello", "echo hello", Some(0), true, false, 1024);
        assert!(out.contains("successfully"));
        assert!(out.contains("```"));
    }

    #[test]
    fn test_truncation() {
        let raw = "a".repeat(10_000);
        let out = process_output(&raw, "echo big", Some(0), true, false, 100);
        assert!(out.contains("too long"));
    }
}
