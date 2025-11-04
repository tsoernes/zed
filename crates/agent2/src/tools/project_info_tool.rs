use std::sync::Arc;

use agent_client_protocol as acp;
use anyhow::Result;
use futures::FutureExt;
use gpui::{App, Entity, SharedString, Task};
use project::Project;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AgentTool, ThreadsDatabase, ToolCallEventStream};

/// Manages project-specific information that persists across chat sessions.
/// This tool allows you to maintain context and learnings about the project structure, conventions, and important details.
/// The information stored here will be automatically included at the start of every new conversation for this project.
///
/// Use this tool to:
/// - Record important project patterns, conventions, or architectural decisions
/// - Note file locations and their purposes
/// - Track key APIs, functions, or classes you've learned about
/// - Document project-specific workflows or build processes
/// - Keep notes that will be useful in future conversations
///
/// Consider updating this information as you learn new things about the project during your work.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ProjectInfoToolInput {
    /// The action to perform on the project info
    #[serde(rename = "action")]
    pub action: ProjectInfoAction,
    /// The content to append or set (required for 'append' and 'set' actions)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectInfoAction {
    /// Append new information to the existing project info
    Append,
    /// Replace the entire project info with new content
    Set,
    /// Read the current project info content
    Read,
    /// Clear all project info for this project
    Clear,
}

pub struct ProjectInfoTool {
    project: Entity<Project>,
    db: futures::future::Shared<Task<Result<Arc<ThreadsDatabase>, Arc<anyhow::Error>>>>,
}

impl ProjectInfoTool {
    pub fn new(
        project: Entity<Project>,
        db: futures::future::Shared<Task<Result<Arc<ThreadsDatabase>, Arc<anyhow::Error>>>>,
    ) -> Self {
        Self { project, db }
    }

    fn get_project_key(&self, cx: &App) -> Arc<str> {
        let worktree_roots = self.project.read(cx).worktree_root_names(cx);
        let project_key = if worktree_roots.is_empty() {
            "default".to_string()
        } else {
            worktree_roots.join(";")
        };
        Arc::from(project_key)
    }
}

impl AgentTool for ProjectInfoTool {
    type Input = ProjectInfoToolInput;
    type Output = String;

    fn name() -> &'static str {
        "project_info"
    }

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Other
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        if let Ok(input) = input {
            match input.action {
                ProjectInfoAction::Append => "Append to project info".into(),
                ProjectInfoAction::Set => "Set project info".into(),
                ProjectInfoAction::Read => "Read project info".into(),
                ProjectInfoAction::Clear => "Clear project info".into(),
            }
        } else {
            "Manage project info".into()
        }
    }

    fn run(
        self: Arc<Self>,
        input: Self::Input,
        _event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<String>> {
        let project_key = self.get_project_key(cx);
        let db_future = self.db.clone();

        cx.spawn(async move |_cx| {
            let db = db_future
                .await
                .map_err(|e| anyhow::anyhow!("Failed to connect to database: {}", e))?;

            match input.action {
                ProjectInfoAction::Read => {
                    let content = db.load_project_info(project_key).await?;
                    Ok(content.unwrap_or_else(|| {
                        "No project info has been stored yet.".to_string()
                    }))
                }
                ProjectInfoAction::Append => {
                    let content_to_append = input.content.ok_or_else(|| {
                        anyhow::anyhow!("Content is required for append action")
                    })?;

                    let existing_content = db.load_project_info(project_key.clone()).await?;
                    let new_content = if let Some(existing) = existing_content {
                        format!("{}\n\n{}", existing, content_to_append)
                    } else {
                        content_to_append
                    };

                    db.save_project_info(project_key, new_content).await?;
                    Ok("Project info updated successfully.".to_string())
                }
                ProjectInfoAction::Set => {
                    let content = input
                        .content
                        .ok_or_else(|| anyhow::anyhow!("Content is required for set action"))?;

                    db.save_project_info(project_key, content).await?;
                    Ok("Project info set successfully.".to_string())
                }
                ProjectInfoAction::Clear => {
                    db.delete_project_info(project_key).await?;
                    Ok("Project info cleared successfully.".to_string())
                }
            }
        })
    }
}
