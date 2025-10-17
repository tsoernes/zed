use crate::schema::json_schema_for;
use action_log::ActionLog;
use anyhow::{Context as _, Result, anyhow};
use assistant_tool::{Tool, ToolResult};
use gpui::{AnyWindowHandle, App, AppContext, Entity, Task};
use language_model::{LanguageModel, LanguageModelRequest, LanguageModelToolSchemaFormat};
use project::Project;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use text::{LineEnding, Rope};
use ui::IconName;
use util::markdown::MarkdownInlineCode;

/// Input for the `save_context` tool.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SaveContextToolInput {
    /// The destination path (project-relative) where the raw message history should be written.
    ///
    /// WARNING: The path MUST start with one of the project's root directories.
    ///
    /// Examples:
    /// - backend/context_dump.json
    /// - frontend/logs/session_001.json
    pub path: String,
}

/// Tool that saves the entire raw current message history (from the active request)
/// to a file in the project. The history is serialized as pretty-printed JSON,
/// preserving roles and content variants (text, thinking, image, tool use/result).
pub struct SaveContextTool;

impl Tool for SaveContextTool {
    fn name(&self) -> String {
        "save_context".into()
    }

    fn description(&self) -> String {
        "Write the agent's full, raw message history from the current request to a project file as JSON.".into()
    }

    fn icon(&self) -> IconName {
        IconName::FileTextOutlined
    }

    fn needs_confirmation(&self, _: &serde_json::Value, _: &Entity<Project>, _: &App) -> bool {
        // This writes to disk, but only within the project; keep friction low.
        // If you want explicit confirmation, change to `true`.
        false
    }

    fn may_perform_edits(&self) -> bool {
        // This tool writes a file in the project.
        true
    }

    fn input_schema(&self, format: LanguageModelToolSchemaFormat) -> Result<serde_json::Value> {
        json_schema_for::<SaveContextToolInput>(format)
    }

    fn ui_text(&self, input: &serde_json::Value) -> String {
        match serde_json::from_value::<SaveContextToolInput>(input.clone()) {
            Ok(input) => format!(
                "Save raw message history to {}",
                MarkdownInlineCode(&input.path)
            ),
            Err(_) => "Save raw message history".to_string(),
        }
    }

    fn run(
        self: Arc<Self>,
        input: serde_json::Value,
        request: Arc<LanguageModelRequest>,
        project: Entity<Project>,
        _action_log: Entity<ActionLog>,
        _model: Arc<dyn LanguageModel>,
        _window: Option<AnyWindowHandle>,
        cx: &mut App,
    ) -> ToolResult {
        let input = match serde_json::from_value::<SaveContextToolInput>(input) {
            Ok(input) => input,
            Err(err) => return Task::ready(Err(anyhow!(err))).into(),
        };

        // Resolve the project path; fail if outside the project.
        let Some(project_path) = project.read(cx).find_project_path(&input.path, cx) else {
            return Task::ready(Err(anyhow!(
                "Destination path {} was outside the project",
                input.path
            )))
            .into();
        };

        // Serialize the entire raw message history to pretty JSON.
        // This preserves the message roles and all content variants.
        let json_text = match serde_json::to_string_pretty(&request.messages) {
            Ok(text) => text,
            Err(err) => return Task::ready(Err(anyhow!(err))).into(),
        };

        // Normalize line endings and prepare Rope for the write operation.
        let mut normalized_text = json_text;
        let line_ending = LineEnding::detect(&normalized_text);
        LineEnding::normalize(&mut normalized_text);
        let rope = Rope::from(normalized_text);

        // Perform the write via the worktree.
        let write_task = project.update(cx, |project, cx| {
            let Some(worktree) = project.worktree_for_id(project_path.worktree_id, cx) else {
                return Task::ready(Err(anyhow!(
                    "No worktree for destination path {}",
                    input.path
                )));
            };

            worktree.update(cx, |worktree, cx| {
                worktree.write_file(project_path.path.clone(), rope, line_ending, cx)
            })
        });

        cx.background_spawn(async move {
            write_task
                .await
                .with_context(|| format!("Writing raw message history to {}", input.path))?;
            Ok(format!("Saved raw message history to {}", input.path).into())
        })
        .into()
    }
}
