use crate::schema::json_schema_for;
use action_log::ActionLog;
use anyhow::Result;
use assistant_tool::{Tool, ToolResult};
use gpui::{AnyWindowHandle, App, Entity, Task};
use language_model::{LanguageModel, LanguageModelRequest, LanguageModelToolSchemaFormat};
use project::Project;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::SystemTime;
use ui::IconName;

use super::enhanced_terminal_tool::jobs;

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct EnhancedTerminalListJobsInput {}

pub struct EnhancedTerminalListJobsTool;

impl EnhancedTerminalListJobsTool {
    pub const NAME: &str = "enhanced_terminal_list_jobs";
}

impl Tool for EnhancedTerminalListJobsTool {
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
        include_str!("./enhanced_terminal_list_jobs/description.md").to_string()
    }

    fn icon(&self) -> IconName {
        IconName::ToolTerminal
    }

    fn input_schema(&self, format: LanguageModelToolSchemaFormat) -> Result<serde_json::Value> {
        json_schema_for::<EnhancedTerminalListJobsInput>(format)
    }

    fn ui_text(&self, _input: &serde_json::Value) -> String {
        "List async terminal jobs".into()
    }

    fn run(
        self: Arc<Self>,
        _input: serde_json::Value,
        _request: Arc<LanguageModelRequest>,
        _project: Entity<Project>,
        _action_log: Entity<ActionLog>,
        _model: Arc<dyn LanguageModel>,
        _window: Option<AnyWindowHandle>,
        _cx: &mut App,
    ) -> ToolResult {
        let now = SystemTime::now();
        let jobs_snapshot = {
            let map = jobs().lock().unwrap();
            map.iter()
                .map(|(jid, rec)| {
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

                    let started_at_secs = rec
                        .started_at
                        .duration_since(std::time::UNIX_EPOCH)
                        .ok()
                        .map(|d| d.as_secs())
                        .unwrap_or(0);

                    let exit_code_json = rec
                        .exit_code
                        .map(serde_json::Value::from)
                        .unwrap_or(serde_json::Value::Null);

                    serde_json::json!({
                        "job_id": jid,
                        "state": state,
                        "exit_code": exit_code_json,
                        "success": rec.success,
                        "truncated": rec.truncated,
                        "canceled": rec.canceled,
                        "runtime_secs": runtime_secs,
                        "preview": rec.output,
                        "command": rec.command,
                        "used_sudo": rec.used_sudo,
                        "dangerous": rec.dangerous,
                        "started_at": started_at_secs
                    })
                })
                .collect::<Vec<_>>()
        };

        let json = serde_json::json!({ "jobs": jobs_snapshot });

        ToolResult {
            output: Task::ready(Ok(json.to_string().into())),
            card: None,
        }
    }
}
