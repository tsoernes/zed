use crate::schema::json_schema_for;
use action_log::ActionLog;
use anyhow::{Result, anyhow};
use assistant_tool::{Tool, ToolResult};
use gpui::AppContext;
use gpui::{AnyWindowHandle, App, Entity, Task};
use language_model::{LanguageModel, LanguageModelRequest, LanguageModelToolSchemaFormat};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use project::Project;

use agent_settings::AgentSettings;
#[cfg(unix)]
use libc;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use settings::Settings as _;
use std::{
    collections::HashMap,
    env,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};
use ui::IconName;
use util::markdown::MarkdownInlineCode;

/// Enhanced terminal safety configuration now sourced from AgentSettings (settings.json / default.json).
/// Environment-variable based configuration has been removed.

pub(crate) const DEFAULT_OUTPUT_LIMIT: usize = 16 * 1024;
pub(crate) const LARGE_OUTPUT_LIMIT: usize = 256 * 1024;

// Detached job registry
#[derive(Debug, Clone)]
pub(crate) struct JobRecord {
    // Original command
    pub(crate) command: String,
    // Timing
    pub(crate) started_at: std::time::SystemTime,
    pub(crate) finished_at: Option<std::time::SystemTime>,
    // Result
    pub(crate) exit_code: Option<i32>,
    pub(crate) success: bool,
    pub(crate) used_sudo: bool,
    // Streaming preview (truncated to output_limit) updated incrementally
    pub(crate) output: String,
    pub(crate) truncated: bool,
    // Full accumulated output (never truncated; surfaced via full_output=true)
    pub(crate) full_output: String,
    // Cancellation flag (semantic plus best-effort signal dispatch on Unix)
    pub(crate) canceled: bool,
    // Whether command matched denylist (informational)
    pub(crate) dangerous: bool,
    // Child pid for real cancellation on Unix
    #[cfg(unix)]
    pub(crate) pid: Option<u32>,
}

static JOB_COUNTER: AtomicU64 = AtomicU64::new(1);
static JOBS: OnceLock<Mutex<HashMap<String, JobRecord>>> = OnceLock::new();

/// Build the effective denylist from settings each time (settings are user‑mutable at runtime).
pub(crate) fn build_effective_denylist(settings: &agent_settings::AgentSettings) -> Vec<String> {
    let mut patterns: Vec<String> = Vec::new();
    if !settings.enhanced_terminal_disable_default_denylist {
        for pat in &[
            "rm -rf /",
            "mkfs",
            ":(){:|:&};:",
            "dd if=",
            "shutdown -h",
            "reboot",
            "chmod 777 /",
            "chown root:",
        ] {
            patterns.push(pat.to_string());
        }
    }
    for pat in &settings.enhanced_terminal_denylist {
        if !pat.is_empty() {
            patterns.push(pat.to_lowercase());
        }
    }
    patterns
}

pub(crate) fn command_matches_any(patterns: &[String], cmd: &str) -> bool {
    let lowered = cmd.to_lowercase();
    patterns.iter().any(|p| lowered.contains(p))
}

pub(crate) fn jobs() -> &'static Mutex<HashMap<String, JobRecord>> {
    JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn new_job_id() -> String {
    format!(
        "enhterm-job-{}",
        JOB_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

pub(crate) fn command_is_dangerous(cmd: &str) -> bool {
    // Conservative pattern list; intentionally simple to minimize false negatives while
    // avoiding over-complication. Caller can override with allow_dangerous=true.
    // NOTE: Patterns are lowercase-matched; update description.md if changed.
    const PATTERNS: &[&str] = &[
        "rm -rf /",
        "mkfs",
        ":(){:|:&};:",
        "dd if=",
        "shutdown -h",
        "reboot",
        "chmod 777 /",
        "chown root:",
    ];
    let lowered = cmd.to_lowercase();
    PATTERNS.iter().any(|p| lowered.contains(p))
}

/// Input for the minimal enhanced terminal tool.
/// This headless-only implementation:
/// - Supports optional sudo
/// - Allows any absolute path as cwd (even outside project)
/// - Allows specifying a shell
/// - Allows adjusting output limit
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct EnhancedTerminalToolInput {
    /// Command to execute (one-liner or multi-line; no embedded cd).
    /// Required unless querying status of an existing detached job.
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
    /// Optional timeout (seconds) for synchronous execution before detaching.
    /// Default 0: no early detachment. If the command exceeds this duration, a job_id is returned.
    #[serde(default)]
    timeout_seconds: Option<u64>,
    /// If true, run the command detached and return immediately with a job id.
    #[serde(default)]
    detach: bool,
    /// When set (and command is empty), return status for the detached job id.
    #[serde(default)]
    job_id: Option<String>,
    /// When true in a status query (command empty + job_id set), return the full (untruncated) output if the job is finished.
    #[serde(default)]
    full_output: bool,
    /// When true in a status query (command empty + job_id set), request cancellation of the running job.
    /// Cancellation attempts a SIGTERM followed by SIGKILL (Unix) best-effort; on non-Unix it marks the job canceled.
    #[serde(default)]
    cancel: bool,
    /// When true and the command is empty, list all known detached jobs and their statuses.
    #[serde(default)]
    list_jobs: bool,
    /// Allow execution to continue even if the command matches a denylisted dangerous pattern.
    #[serde(default)]
    allow_dangerous: bool,
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

        // STATUS / CONTROL MODE:
        // If command is empty AND job_id provided -> status / cancel / full_output.
        // If command is empty AND list_jobs=true -> list all known jobs.
        // Cancellation now attempts real process termination on Unix (best-effort).
        if input.command.trim().is_empty() {
            if input.list_jobs {
                let now = std::time::SystemTime::now();
                let list = {
                    let map = jobs().lock().unwrap();
                    let mut rows = Vec::new();
                    for (jid, rec) in map.iter() {
                        let state = if rec.canceled {
                            "canceled"
                        } else if rec.finished_at.is_some() {
                            "finished"
                        } else {
                            "running"
                        };
                        let runtime_secs = match rec.finished_at {
                            Some(finish) => finish
                                .duration_since(rec.started_at)
                                .ok()
                                .map(|d| d.as_secs())
                                .unwrap_or(0),
                            None => now
                                .duration_since(rec.started_at)
                                .ok()
                                .map(|d| d.as_secs())
                                .unwrap_or(0),
                        };
                        let exit = rec
                            .exit_code
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "null".into());
                        let preview = if rec.output.is_empty() {
                            "\"\"".to_string()
                        } else {
                            serde_json::to_string(&rec.output.chars().take(200).collect::<String>())
                                .unwrap_or("\"<encoding error>\"".into())
                        };
                        let cmd = serde_json::to_string(&rec.command)
                            .unwrap_or("\"<encoding error>\"".into());
                        rows.push(format!(
                            "{{\"job_id\":{},\"state\":\"{}\",\"exit_code\":{},\"success\":{},\"truncated\":{},\"canceled\":{},\"runtime_secs\":{},\"preview\":{},\"command\":{}}}",
                            serde_json::to_string(jid).unwrap_or("\"<encoding error>\"".into()),
                            state, exit, rec.success, rec.truncated, rec.canceled, runtime_secs, preview, cmd
                        ));
                    }
                    format!("{{\"jobs\":[{}]}}", rows.join(","))
                };
                return ToolResult {
                    output: Task::ready(Ok(list.into())),
                    card: None,
                };
            }

            if let Some(job_id) = &input.job_id {
                // Optional cancellation
                if input.cancel {
                    let mut map = jobs().lock().unwrap();
                    if let Some(rec) = map.get_mut(job_id) {
                        if rec.finished_at.is_none() {
                            rec.canceled = true;
                            #[cfg(unix)]
                            {
                                if let Some(pid) = rec.pid {
                                    // Send SIGTERM first
                                    unsafe {
                                        let _ = libc::kill(pid as i32, libc::SIGTERM);
                                    }
                                    // Escalate to SIGKILL after 5s if still not finished
                                    std::thread::spawn(move || {
                                        std::thread::sleep(std::time::Duration::from_secs(5));
                                        unsafe {
                                            let _ = libc::kill(pid as i32, libc::SIGKILL);
                                        }
                                    });
                                }
                            }
                        }
                    }
                }

                let status = {
                    let map = jobs().lock().unwrap();
                    map.get(job_id).cloned()
                };

                let msg = match status {
                    None => format!("{{\"job_id\":\"{job_id}\",\"state\":\"not_found\"}}"),
                    Some(rec) => {
                        let state = if rec.canceled {
                            "canceled"
                        } else if rec.finished_at.is_some() {
                            "finished"
                        } else {
                            "running"
                        };
                        let now = std::time::SystemTime::now();
                        let runtime_secs = match rec.finished_at {
                            Some(finish) => finish
                                .duration_since(rec.started_at)
                                .ok()
                                .map(|d| d.as_secs())
                                .unwrap_or(0),
                            None => now
                                .duration_since(rec.started_at)
                                .ok()
                                .map(|d| d.as_secs())
                                .unwrap_or(0),
                        };
                        // Full output fetch (only if finished/canceled and requested)
                        if input.full_output && (state == "finished" || state == "canceled") {
                            let fo = serde_json::to_string(&rec.full_output)
                                .unwrap_or("\"<encoding error>\"".into());
                            let exit = rec
                                .exit_code
                                .map(|c| c.to_string())
                                .unwrap_or_else(|| "null".into());
                            return ToolResult {
                                output: Task::ready(Ok(format!(
                                    "{{\"job_id\":\"{job_id}\",\"state\":\"{state}\",\"exit_code\":{exit},\"success\":{},\"truncated\":{},\"canceled\":{},\"runtime_secs\":{},\"full_output\":{}}}",
                                    rec.success, rec.truncated, rec.canceled, runtime_secs, fo
                                ).into())),
                                card: None,
                            };
                        }

                        let exit = rec
                            .exit_code
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "null".into());
                        let success = rec.success;
                        let truncated = rec.truncated;
                        let preview = if rec.output.is_empty() {
                            "".to_string()
                        } else {
                            let snippet = rec.output.chars().take(400).collect::<String>();
                            serde_json::to_string(&snippet).unwrap_or("\"<encoding error>\"".into())
                        };
                        // Include previously unused fields to surface them and avoid dead_code warnings.
                        let command_json = serde_json::to_string(&rec.command)
                            .unwrap_or("\"<encoding error>\"".into());
                        let started = rec
                            .started_at
                            .duration_since(std::time::UNIX_EPOCH)
                            .ok()
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        let used_sudo = rec.used_sudo;
                        let dangerous = rec.dangerous;
                        format!(
                            "{{\"job_id\":\"{job_id}\",\"state\":\"{state}\",\"exit_code\":{exit},\"success\":{},\"truncated\":{},\"canceled\":{},\"preview\":{},\"command\":{command_json},\"started_at\":{started},\"runtime_secs\":{},\"used_sudo\":{},\"dangerous\":{}}}",
                            success,
                            truncated,
                            rec.canceled,
                            preview,
                            started,
                            used_sudo,
                            dangerous
                        )
                    }
                };
                return ToolResult {
                    output: Task::ready(Ok(msg.into())),
                    card: None,
                };
            }
        }

        // DETACH MODE:
        if input.detach {
            // Fetch settings for enhanced terminal safety
            let settings = AgentSettings::get_global(cx);
            // allow_dangerous flag only honored if globally enabled
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
            let job_id = new_job_id();

            {
                let mut map = jobs().lock().unwrap();
                map.insert(
                    job_id.clone(),
                    JobRecord {
                        command: input.command.clone(),
                        started_at: std::time::SystemTime::now(),
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

            // Streaming + background collection
            cx.background_spawn({
                let job_id = job_id.clone();
                async move {
                    let pty_system = native_pty_system();
                    let (program, args) = shell_command_parts(&final_script, &input.shell);
                    let mut cmd = CommandBuilder::new(program);
                    cmd.args(args);
                    if let Some(cwd) = &working_dir {
                        cmd.cwd(cwd);
                    }
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

                    // Streaming loop: read small chunks, update preview
                    let mut buf = [0u8; 4096];
                    let mut full_raw = String::new();
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                let chunk = String::from_utf8_lossy(&buf[..n]);
                                full_raw.push_str(&chunk);
                                // Update truncated preview
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
                                if rec.full_output.len() > effective_limit {
                                    // preview already updated in loop
                                } else {
                                    rec.output = rec.full_output.clone();
                                }
                                rec.finished_at = Some(std::time::SystemTime::now());
                            }
                        }
                    }

                    Ok::<(), anyhow::Error>(())
                }
            })
            .detach();

            let response = format!("{{\"job_id\":\"{job_id}\",\"state\":\"running\"}}");
            return ToolResult {
                output: Task::ready(Ok(response.into())),
                card: None,
            };
        }

        // Normal (blocking) mode:
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

        let task = cx.background_spawn(async move {
            let pty_system = native_pty_system();
            let (program, args) = shell_command_parts(&final_script, &input.shell);
            let mut cmd = CommandBuilder::new(program);
            cmd.args(args);

            if let Some(cwd) = &working_dir {
                cmd.cwd(cwd);
            }

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

            // Incremental read to allow future partial streaming.
            // Currently we accumulate, but we can emit partial chunks where marked.
            let mut raw = String::new();
            {
                let mut buf = [0u8; 4096];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            let chunk = String::from_utf8_lossy(&buf[..n]);
                            raw.push_str(&chunk);
                        }
                        Err(e) => {
                            raw.push_str(&format!("\n[read error: {e}]"));
                            break;
                        }
                    }
                }
            }
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
