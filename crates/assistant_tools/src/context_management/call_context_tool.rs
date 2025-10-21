use std::sync::Arc;

use crate::context_management::{ListHistoryTool, MemoryTool};
use crate::schema::json_schema_for;
use action_log::ActionLog;
use anyhow::{Result, anyhow};
use assistant_tool::{Tool, ToolResult};
use gpui::{AnyWindowHandle, App, Entity, Task};
use language_model::{LanguageModel, LanguageModelRequest, LanguageModelToolSchemaFormat};
use project::Project;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ui::IconName;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContextToolName {
    ListHistory,
    Memory,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CallContextToolInput {
    /// The name of the context tool to invoke
    name: ContextToolName,

    /// The arguments to pass to the specified tool
    arguments: Option<Value>,
}

pub struct CallContextTool;

impl Tool for CallContextTool {
    fn name(&self) -> String {
        "call_context_tool".into()
    }

    fn needs_confirmation(
        &self,
        input: &serde_json::Value,
        project: &Entity<Project>,
        cx: &App,
    ) -> bool {
        // Delegate to the underlying tool
        if let Ok(input) = serde_json::from_value::<CallContextToolInput>(input.clone()) {
            match input.name {
                ContextToolName::ListHistory => {
                    let list_history_tool = ListHistoryTool;
                    let args = input.arguments.unwrap_or_else(|| serde_json::json!({}));
                    list_history_tool.needs_confirmation(&args, project, cx)
                }
                ContextToolName::Memory => {
                    let memory_tool = MemoryTool;
                    let args = input.arguments.unwrap_or_else(|| serde_json::json!({}));
                    memory_tool.needs_confirmation(&args, project, cx)
                }
            }
        } else {
            false
        }
    }

    fn may_perform_edits(&self) -> bool {
        false
    }

    fn description(&self) -> String {
        "A unified entry point for context management tools (list_history, memory) using a generic schema."
            .into()
    }

    fn icon(&self) -> IconName {
        IconName::Settings
    }

    fn input_schema(&self, format: LanguageModelToolSchemaFormat) -> Result<serde_json::Value> {
        json_schema_for::<CallContextToolInput>(format)
    }

    fn ui_text(&self, input: &serde_json::Value) -> String {
        if let Ok(input) = serde_json::from_value::<CallContextToolInput>(input.clone()) {
            match input.name {
                ContextToolName::ListHistory => {
                    let list_history_tool = ListHistoryTool;
                    let args = input.arguments.unwrap_or_else(|| serde_json::json!({}));
                    format!("Context: {}", list_history_tool.ui_text(&args))
                }
                ContextToolName::Memory => {
                    let memory_tool = MemoryTool;
                    let args = input.arguments.unwrap_or_else(|| serde_json::json!({}));
                    format!("Context: {}", memory_tool.ui_text(&args))
                }
            }
        } else {
            "Context tool operation".to_string()
        }
    }

    fn run(
        self: Arc<Self>,
        input: serde_json::Value,
        request: Arc<LanguageModelRequest>,
        project: Entity<Project>,
        action_log: Entity<ActionLog>,
        model: Arc<dyn LanguageModel>,
        window: Option<AnyWindowHandle>,
        cx: &mut App,
    ) -> ToolResult {
        let input: CallContextToolInput = match serde_json::from_value(input) {
            Ok(input) => input,
            Err(err) => return Task::ready(Err(anyhow!("Invalid input: {}", err))).into(),
        };

        match input.name {
            ContextToolName::ListHistory => {
                let list_history_tool = Arc::new(ListHistoryTool);
                let args = input.arguments.unwrap_or_else(|| serde_json::json!({}));
                list_history_tool.run(args, request, project, action_log, model, window, cx)
            }
            ContextToolName::Memory => {
                let memory_tool = Arc::new(MemoryTool);
                let args = input.arguments.unwrap_or_else(|| serde_json::json!({}));
                memory_tool.run(args, request, project, action_log, model, window, cx)
            }
        }
    }
}
