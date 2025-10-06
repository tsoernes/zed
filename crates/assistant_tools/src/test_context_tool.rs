use std::sync::Arc;

use crate::schema::json_schema_for;
use action_log::ActionLog;
use anyhow::Result;
use assistant_tool::{Tool, ToolResult, ToolResultOutput};
use gpui::{AnyWindowHandle, App, Entity, Task};
use language_model::{LanguageModel, LanguageModelRequest, LanguageModelToolSchemaFormat};
use log;
use project::Project;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ui::IconName;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct TestContextToolInput {}

pub struct TestContextTool;

impl Tool for TestContextTool {
    fn name(&self) -> String {
        "test_context".into()
    }

    fn needs_confirmation(&self, _: &serde_json::Value, _: &Entity<Project>, _: &App) -> bool {
        false
    }

    fn may_perform_edits(&self) -> bool {
        false
    }

    fn description(&self) -> String {
        "A test tool to verify context management tools can be registered".into()
    }

    fn icon(&self) -> IconName {
        IconName::Info
    }

    fn input_schema(&self, format: LanguageModelToolSchemaFormat) -> Result<serde_json::Value> {
        json_schema_for::<TestContextToolInput>(format)
    }

    fn ui_text(&self, _input: &serde_json::Value) -> String {
        "Test context tool".to_string()
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
        log::error!("TestContextTool::run() called - THIS SHOULD APPEAR IN LOGS");
        let output = "Test context tool executed successfully! If you can see this, tool registration is working.".to_string();
        Task::ready(Ok(ToolResultOutput::from(output))).into()
    }
}
