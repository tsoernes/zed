use crate::schema::json_schema_for;
use action_log::ActionLog;
use anyhow::{anyhow, Result};
use assistant_tool::{Tool, ToolResult};
use gpui::{AnyWindowHandle, App, AppContext, Entity, Task};
use language_model::{LanguageModel, LanguageModelRequest, LanguageModelToolSchemaFormat};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use project::Project;

use agent_settings::AgentSettings;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use settings::Settings as _;
use std::{
    env,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};
use ui::IconName;
use util::markdown::MarkdownInlineCode;

use super::enhanced_terminal_tool::{
    build_effective_denylist, command_is_dangerous, command_matches_any, jobs, new_job_id,
    JobRecord, DEFAULT_OUTPUT_LIMIT, LARGE_OUTPUT_LIMIT,
};

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct EnhancedTerminalAsyncToolInput {
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
    /// Optional timeout (seconds) to wait for completion before returning a job id.
    /// Default 0: return immediately with job id.
    #[serde(default)]
    timeout_seconds: Option<u64>,
    /// Optional whitelist of environment variables to pass through into the command process.
    /// Variables absent from the host environment are skipped.
    #[serde(default)]
    env_whitelist: Option<Vec<String>>,
    /// Allow execution even if the command matches a denylisted dangerous pattern.
    /// Requires global setting agent.enhanced_terminal_allow_dangerous=true to take effect.
    #[serde(default)]
    allow_dangerous: bool,
}

fn default_cwd() -> String {
    ".".to_string()
}

pub struct EnhancedTerminalAsyncTool;

impl EnhancedTerminalAsyncTool {
    pub const NAME: &str = "enhanced_terminal_async";
}

impl Tool for EnhancedTerminalAsyncTool {
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
        // Dedicated description for the async variant can be added here if desired.
        // For now, keep it succinct.
        "Execute shell commands asynchronously with optional timeout-based immediate result or job id, structured JSON responses, and environment variable whitelisting."
            .into()
    }

    fn icon(&self) -> IconName {
        IconName::ToolTerminal
    }

    fn input_schema(&self, format: LanguageModelToolSchemaFormat) -> Result<serde_json::Value> {
        json_schema_for::<EnhancedTerminalAsyncToolInput>(format)
    }

    fn ui_text(&self, input: &serde_json::Value) -> String {
        match serde_json::from_value::<EnhancedTerminalAsyncToolInput>(input.clone()) {
            Ok(input) => {
                let mut lines = input.command.lines();
                let first = lines.next().unwrap_or_default();
                let rest = lines.count();
                let prefix = if input.use_sudo { "sudo " } else { "" };
                match rest {
                    0 => MarkdownInlineCode(&format!("{prefix}{first}")).to_string(),
                    1 => MarkdownInlineCode(&format!("{prefix}{first} - 1 more line")).to_string(),
                    n => MarkdownInlineCode(&format!("{prefix}{first} - {n} more lines")).to_string(),
                }
            }
            Err(_) => "Run enhanced terminal command (async)".into(),
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
        let input: EnhancedTerminalAsyncToolInput = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return Task::ready(Err(anyhow!(e))).into(),
        };

        // Safety configuration
        let settings = AgentSettings::get_global(cx);
        if input.allow_dangerous && !settings.enhanced_terminal_allow_dangerous {
            return ToolResult {
                output: Task::ready(Err(anyhow!(
                    "allow_dangerous=true was specified but is not permitted (enable enhanced_terminal_allow_dangerous in settings to allow)."
                ))),
                card: None,
            };
        }
        let effective_allow_dangerous =
            input.allow_dangerous && settings.enhanced_terminal_allow_dangerous;
        let denylist = build_effective_denylist(&settings);

        if command_is_dangerous(&input.command) && !effective_allow_dangerous {
            return ToolResult {
                output: Task::ready(Err(anyhow!(
                    "Command denied by safety policy (matches dangerous pattern). Enable allow_dangerous in the tool input AND enhanced_terminal_allow_dangerous in settings to proceed."
                ))),
                card: None,
            };
        }
        if command_matches_any(&denylist, &input.command) && !effective_allow_dangerous {
            return ToolResult {
                output: Task::ready(Err(anyhow!(
                    "Command rejected by safety policy (matched denylist). Update enhanced_terminal_denylist / settings or explicitly allow (requires global enhanced_terminal_allow_dangerous + allow_dangerous=true)."
                ))),
                card: None,
            };
        }

        let working_dir = match resolve_working_directory(&input.cwd, &project, cx) {
            Ok(dir) => dir,
            Err(err) => return Task::ready(Err(err)).into(),
        };
        let effective_limit = input.output_limit.unwrap_or(if input.use_sudo {
            LARGE_OUTPUT_LIMIT
        } else {
            DEFAULT_OUTPUT_LIMIT
        });
        let final_script = build_final_command(&input);

        // Create a new job and start background execution with streaming preview.
        let job_id = new_job_id();
        {
            let mut map = jobs().lock().unwrap();
            map.insert(
                job_id.clone(),
                JobRecord {
                    command: input.command.clone(),
                    started_at: SystemTime::now(),
                    finished_at: None,
                    exit_code: None,
                    success: false,
                    used_sudo: input.use_sudo,
                    output: String::new(),
                    truncated: false,
                    full_output: String::new(),
                    canceled: false,
                    dangerous: command_is_dangerous(&input.command),
                    #[cfg(unix)]
                    pid: None,
                },
            );
        }

        // Background worker to execute the command and update job registry.
        cx.background_spawn({
            let job_id = job_id.clone();
            let working_dir = working_dir.clone();
            let shell = input.shell.clone();
            let env_whitelist = input.env_whitelist.clone().unwrap_or_default();
            async move {
                let pty_system = native_pty_system();
                let (program, args) = shell_command_parts(&final_script, &shell);
                let mut cmd = CommandBuilder::new(program);
                cmd.args(args);

                // Resolve working directory
                if let Some(cwd) = &working_dir {
                    cmd.cwd(cwd);
                }

                // Baseline safe env
                if cfg!(unix) {
                    cmd.env("PAGER", "cat");
                }
                if let Ok(home) = env::var("HOME") {
                    cmd.env("HOME", home);
                }

                // Apply env whitelist (best-effort)
                for key in env_whitelist {
                    if let Ok(val) = env::var(&key) {
                        cmd.env(key, val);
                    }
                }

                // Open PTY and spawn
                let pair = pty_system.openpty(PtySize {
                    rows: 24,
                    cols: 80,
                    ..Default::default()
                })?;
                let mut child = pair.slave.spawn_command(cmd)?;
                #[cfg(unix)]
                {
                    if let Some(pid) = child.process_id() {
                        let mut map = jobs().lock().unwrap();
                        if let Some(rec) = map.get_mut(&job_id) {
                            rec.pid = Some(pid);
                        }
                    }
                }
                let mut reader = pair.master.try_clone_reader()?;
                drop(pair);

                // Stream output into registry
                let mut buf = [0u8; 4096];
                let mut full_raw = String::new();
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            let chunk = String::from_utf8_lossy(&buf[..n]);
                            full_raw.push_str(&chunk);
                            let mut map = jobs().lock().unwrap();
                            if let Some(rec) = map.get_mut(&job_id) {
                                if rec.finished_at.is_none() {
                                    if full_raw.len() > effective_limit {
                                        let mut preview = full_raw.clone();
                                        let mut end_ix = effective_limit;
                                        while !preview.is_char_boundary(end_ix) && end_ix > 0 {
                                            end_ix -= 1;
                                        }
                                        preview.truncate(end_ix);
                                        rec.output = preview;
                                        rec.truncated = true;
                                    } else {
                                        rec.output = full_raw.clone();
                                        rec.truncated = false;
                                    }
                                    rec.full_output = full_raw.clone();
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }

                // Wait for exit and update registry
                let status = child.wait()?;
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

                {
                    let mut map = jobs().lock().unwrap();
                    if let Some(rec) = map.get_mut(&job_id) {
                        if !rec.canceled {
                            rec.exit_code = exit_code;
                            rec.success = success && exit_code.unwrap_or(1) == 0;
                            if rec.full_output.len() <= effective_limit {
                                rec.output = rec.full_output.clone();
                            }
                            rec.finished_at = Some(SystemTime::now());
                        }
                    }
                }

                Ok::<(), anyhow::Error>(())
            }
        })
        .detach();

        // Build the task that either returns a job id immediately (timeout 0),
        // or waits up to timeout_seconds and returns a structured result if completed, else job id.
        let timeout_secs = input.timeout_seconds.unwrap_or(0);
        let task = cx.background_spawn(async move {
            if timeout_secs == 0 {
                return Ok(
                    serde_json::json!({
                        "job_id": job_id,
                        "state": "running"
                    })
                    .to_string()
                    .into(),
                );
            }

            let start = Instant::now();
            loop {
                let rec_opt = {
                    let map = jobs().lock().unwrap();
                    map.get(&job_id).cloned()
                };

                if let Some(rec) = rec_opt {
                    // Completed?
                    if let Some(finished_at) = rec.finished_at {
                        let runtime_secs = finished_at
                            .duration_since(rec.started_at)
                            .ok()
                            .map(|d| d.as_secs())
                            .unwrap_or(0);

                        let output_preview = rec.output.clone();
                        let exit = rec.exit_code.map(|c| serde_json::Value::from(c)).unwrap_or(serde_json::Value::Null);

                        let json = serde_json::json!({
                            "job_id": job_id,
                            "state": "finished",
                            "exit_code": exit,
                            "success": rec.success,
                            "truncated": rec.truncated,
                            "runtime_secs": runtime_secs,
                            "output": output_preview,
                            "used_sudo": rec.used_sudo,
                            "dangerous": rec.dangerous
                        });
                        return Ok(json.to_string().into());
                    }

                    // Still running, check timeout
                    if start.elapsed() >= Duration::from_secs(timeout_secs) {
                        return Ok(
                            serde_json::json!({
                                "job_id": job_id,
                                "state": "running"
                            })
                            .to_string()
                            .into(),
                        );
                    }
                } else {
                    // Job disappeared from registry (unlikely): report not_found
                    return Ok(
                        serde_json::json!({
                            "job_id": job_id,
                            "state": "not_found"
                        })
                        .to_string()
                        .into(),
                    );
                }

                std::thread::sleep(Duration::from_millis(100));
            }
        });

        ToolResult {
            output: task,
            card: None,
        }
    }
}

fn resolve_working_directory(
    cd: &str,
    project: &Entity<Project>,
    cx: &mut App,
) -> Result<Option<PathBuf>> {
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

fn build_final_command(input: &EnhancedTerminalAsyncToolInput) -> String {
    let mut command = input.command.clone();

    if input.use_sudo {
        command = format!("sudo -S {}", command);
    }

    if let Some(shell) = &input.shell {
        let shell_path = resolve_shell_path(shell);
        command = format!("{} -c '{}'", shell_path, command.replace('\'', "'\\''"));
    }

    command
}

fn shell_command_parts(final_script: &str, shell: &Option<String>) -> (String, Vec<String>) {
    match shell {
        Some(s) => {
            let resolved = resolve_shell_path(s);
            (resolved, vec!["-c".into(), final_script.into()])
        }
        None => {
            // Use system default. On Unix, prefer /bin/sh; otherwise spawn through default.
            #[cfg(unix)]
            {
                ("/bin/sh".into(), vec!["-lc".into(), final_script.into()])
            }
            #[cfg(not(unix))]
            {
                ("cmd".into(), vec!["/C".into(), final_script.into()])
            }
        }
    }
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
