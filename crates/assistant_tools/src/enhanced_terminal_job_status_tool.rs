use crate::schema::json_schema_for;
use action_log::ActionLog;
use anyhow::{Result, anyhow};
use assistant_tool::{Tool, ToolResult};
use gpui::{AnyWindowHandle, App, Entity, Task};
use language_model::{LanguageModel, LanguageModelRequest, LanguageModelToolSchemaFormat};
use project::Project;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use ui::IconName;

#[cfg(unix)]
use libc;

use super::enhanced_terminal_tool::jobs;

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct EnhancedTerminalJobStatusInput {
    job_id: String,
    #[serde(default)]
    full_output: bool,
    #[serde(default)]
    cancel: bool,
}

pub struct EnhancedTerminalJobStatusTool;

impl EnhancedTerminalJobStatusTool {
    pub const NAME: &str = "enhanced_terminal_job_status";
}

impl Tool for EnhancedTerminalJobStatusTool {
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
        include_str!("./enhanced_terminal_job_status/description.md").to_string()
    }

    fn icon(&self) -> IconName {
        IconName::ToolTerminal
    }

    fn input_schema(&self, format: LanguageModelToolSchemaFormat) -> Result<serde_json::Value> {
        json_schema_for::<EnhancedTerminalJobStatusInput>(format)
    }

    fn ui_text(&self, input: &serde_json::Value) -> String {
        match serde_json::from_value::<EnhancedTerminalJobStatusInput>(input.clone()) {
            Ok(input) => {
                let mut parts = vec![format!("job {}", input.job_id)];
                if input.cancel {
                    parts.push("cancel".into());
                }
                if input.full_output {
                    parts.push("full output".into());
                }
                parts.join(" • ")
            }
            Err(_) => "Get async terminal job status".into(),
        }
    }

    fn run(
        self: Arc<Self>,
        input: serde_json::Value,
        _request: Arc<LanguageModelRequest>,
        _project: Entity<Project>,
        _action_log: Entity<ActionLog>,
        _model: Arc<dyn LanguageModel>,
        _window: Option<AnyWindowHandle>,
        _cx: &mut App,
    ) -> ToolResult {
        let input: EnhancedTerminalJobStatusInput = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return Task::ready(Err(anyhow!(e))).into(),
        };

        // Cancellation request (best-effort). If job running, mark canceled and signal on Unix.
        if input.cancel {
            let mut map = jobs().lock().unwrap();
            if let Some(rec) = map.get_mut(&input.job_id) {
                if rec.finished_at.is_none() {
                    rec.canceled = true;
                    #[cfg(unix)]
                    {
                        if let Some(pid) = rec.pid {
                            // SIGTERM first; escalate to SIGKILL after ~5s in a separate thread
                            unsafe {
                                let _ = libc::kill(pid as i32, libc::SIGTERM);
                            }
                            let pid_kill = pid;
                            std::thread::spawn(move || {
                                std::thread::sleep(Duration::from_secs(5));
                                unsafe {
                                    let _ = libc::kill(pid_kill as i32, libc::SIGKILL);
                                }
                            });
                        }
                    }
                }
            }
        }

        let status = {
            let map = jobs().lock().unwrap();
            map.get(&input.job_id).cloned()
        };

        let json = match status {
            None => serde_json::json!({
                "job_id": input.job_id,
                "state": "not_found",
            }),
            Some(rec) => {
                let state = if rec.canceled {
                    "canceled"
                } else if rec.finished_at.is_some() {
                    "finished"
                } else {
                    "running"
                };

                let now = SystemTime::now();
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
                    .map(serde_json::Value::from)
                    .unwrap_or(serde_json::Value::Null);

                if input.full_output && (state == "finished" || state == "canceled") {
                    serde_json::json!({
                        "job_id": input.job_id,
                        "state": state,
                        "exit_code": exit,
                        "success": rec.success,
                        "truncated": rec.truncated,
                        "canceled": rec.canceled,
                        "runtime_secs": runtime_secs,
                        "full_output": rec.full_output,
                        "used_sudo": rec.used_sudo,
                        "dangerous": rec.dangerous
                    })
                } else {
                    serde_json::json!({
                        "job_id": input.job_id,
                        "state": state,
                        "exit_code": exit,
                        "success": rec.success,
                        "truncated": rec.truncated,
                        "canceled": rec.canceled,
                        "runtime_secs": runtime_secs,
                        "preview": rec.output,
                        "used_sudo": rec.used_sudo,
                        "dangerous": rec.dangerous
                    })
                }
            }
        };

        ToolResult {
            output: Task::ready(Ok(json.to_string().into())),
            card: None,
        }
    }
}
