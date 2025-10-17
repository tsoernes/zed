use crate::schema::json_schema_for;
use action_log::ActionLog;
use anyhow::{Context as _, Result, anyhow};
use assistant_tool::{Tool, ToolResult};
use chrono::Utc;
use gpui::{AnyWindowHandle, App, AppContext, Entity, Task};
use language_model::{LanguageModel, LanguageModelRequest, LanguageModelToolSchemaFormat};
use project::Project;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use text::{LineEnding, Rope};
use ui::IconName;
use util::markdown::MarkdownInlineCode;

/// Input for the `save_context` tool.
///
/// - `path` is the destination project-relative path to write to.
/// - `format` may be `"markdown"` (default) or `"json"`. Markdown output will
///   include a metadata block and a human-readable section followed by raw JSON.
///   JSON output will be a single object with `metadata` and `messages`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SaveContextToolInput {
    /// The destination path (project-relative) where the history should be written.
    ///
    /// WARNING: The path MUST start with one of the project's root directories.
    pub path: String,

    /// Output format: "markdown" or "json". Defaults to "markdown".
    #[serde(default = "default_format")]
    pub format: String,
}

fn default_format() -> String {
    "markdown".to_string()
}

pub struct SaveContextTool;

impl Tool for SaveContextTool {
    fn name(&self) -> String {
        "save_context".into()
    }

    fn description(&self) -> String {
        "Write the agent's full, raw message history from the current request to a project file. Defaults to Markdown with metadata; JSON is also available.".into()
    }

    fn icon(&self) -> IconName {
        IconName::FileTextOutlined
    }

    fn needs_confirmation(&self, _: &serde_json::Value, _: &Entity<Project>, _: &App) -> bool {
        // Writes into the project; keep permission low friction but can be changed.
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
                "Save conversation to {} ({})",
                MarkdownInlineCode(&input.path),
                input.format
            ),
            Err(_) => "Save conversation".to_string(),
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

        // Build metadata
        let metadata = json!({
            "thread_id": request.thread_id,
            "prompt_id": request.prompt_id,
            "intent": request.intent.as_ref().map(|i| format!("{:?}", i)),
            "mode": request.mode.as_ref().map(|m| format!("{:?}", m)),
            "tools_count": request.tools.len(),
            "tool_choice": request.tool_choice.as_ref().map(|t| format!("{:?}", t)),
            "stop": request.stop,
            "temperature": request.temperature,
            "thinking_allowed": request.thinking_allowed,
            "message_count": request.messages.len(),
            "generated_at": Utc::now().to_rfc3339(),
        });

        // Serialize raw messages (used in both formats)
        let raw_json = match serde_json::to_string_pretty(&request.messages) {
            Ok(j) => j,
            Err(err) => return Task::ready(Err(anyhow!(err))).into(),
        };

        // Prepare output text
        let output_text = if input.format.to_lowercase() == "json" {
            // JSON format: single object with metadata and messages
            match serde_json::to_string_pretty(&json!({
                "metadata": metadata,
                "messages": request.messages,
            })) {
                Ok(text) => text,
                Err(err) => return Task::ready(Err(anyhow!(err))).into(),
            }
        } else {
            // Markdown format (default)
            // Human-oriented header + metadata block + brief per-message listing + raw JSON block
            let mut md = String::new();
            md.push_str("# Conversation Dump\n\n");

            // Metadata as JSON code fence for machine-readability
            md.push_str("## Metadata\n\n");
            md.push_str("```json\n");
            md.push_str(&serde_json::to_string_pretty(&metadata).unwrap_or_default());
            md.push_str("\n```\n\n");

            // Human readable excerpt of messages
            md.push_str("## Messages (preview)\n\n");
            for (idx, message) in request.messages.iter().enumerate() {
                let role = format!("{:?}", message.role);
                md.push_str(&format!("### Message {} — {}\n\n", idx, role));

                // Build a simple textual representation by concatenating text-like parts.
                let content_str = message
                    .content
                    .iter()
                    .map(|c| match c {
                        language_model::MessageContent::Text(text) => text.clone(),
                        language_model::MessageContent::Thinking { text, .. } => {
                            format!("(Thinking) {}", text)
                        }
                        language_model::MessageContent::RedactedThinking(text) => {
                            format!("(RedactedThinking) {}", text)
                        }
                        language_model::MessageContent::Image(_) => "[Image]".to_string(),
                        language_model::MessageContent::ToolUse(use_) => {
                            format!(
                                "(ToolUse) {}",
                                serde_json::to_string(&use_.name).unwrap_or_default()
                            )
                        }
                        language_model::MessageContent::ToolResult(result) => {
                            if let Some(output) = &result.output {
                                format!("(ToolResult) {:?}", output)
                            } else {
                                let s = result.content.to_str().unwrap_or("[ToolResult]");
                                format!("(ToolResult) {}", s)
                            }
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n");

                // Limit preview length for readability
                let preview = if content_str.len() > 1000 {
                    format!("{}...", &content_str[..1000])
                } else {
                    content_str
                };

                // Escape triple backticks in preview
                let preview = preview.replace("```", "`` `");

                md.push_str(&preview);
                md.push_str("\n\n");
            }

            // Raw JSON for fidelity
            md.push_str("## Raw JSON\n\n");
            md.push_str("```json\n");
            md.push_str(&raw_json);
            md.push_str("\n```\n");

            md
        };

        // Normalize line endings and prepare Rope for the write operation.
        let mut normalized_text = output_text;
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
                .with_context(|| format!("Writing conversation to {}", input.path))?;
            Ok(format!("Saved conversation to {}", input.path).into())
        })
        .into()
    }
}
