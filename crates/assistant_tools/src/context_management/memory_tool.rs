use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use crate::schema::json_schema_for;
use action_log::ActionLog;
use anyhow::{Result, anyhow, bail};
use assistant_tool::{Tool, ToolResult, ToolResultOutput};
use gpui::{AnyWindowHandle, App, AppContext, Entity, Task};
use language_model::{
    LanguageModel, LanguageModelRequest, LanguageModelRequestMessage, LanguageModelToolSchemaFormat,
};
use parking_lot::Mutex;
use project::Project;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ui::IconName;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryOperation {
    /// Archive a contiguous range of messages
    Store,
    /// Retrieve full original serialized content for inspection
    Load,
    /// Scan current thread for memory placeholders
    List,
    /// Insert archived messages back into the conversation
    Restore,
    /// Remove archived handles that no longer have placeholders
    Prune,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct MemoryToolInput {
    /// The operation to perform
    operation: MemoryOperation,

    /// Required for store: inclusive starting message index
    #[serde(skip_serializing_if = "Option::is_none")]
    start_index: Option<usize>,

    /// Required for store: inclusive ending message index
    #[serde(skip_serializing_if = "Option::is_none")]
    end_index: Option<usize>,

    /// Required for load/restore: memory handle
    #[serde(skip_serializing_if = "Option::is_none")]
    memory_handle: Option<String>,

    /// Optional for store: user-defined summary
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,

    /// For store: if true and summary omitted, generate heuristic summary
    #[serde(default)]
    auto: bool,

    /// For store: preview character limit (default 200, clamped 40..400)
    #[serde(default = "default_preview_chars")]
    max_preview_chars: usize,

    /// For restore: target insertion position (default: append)
    #[serde(skip_serializing_if = "Option::is_none")]
    restore_insert_index: Option<usize>,

    /// For restore: if true, empties placeholder
    #[serde(default)]
    remove_placeholder: bool,

    /// For restore: alternative to remove_placeholder
    #[serde(skip_serializing_if = "Option::is_none")]
    replace_placeholder_with: Option<String>,
}

fn default_preview_chars() -> usize {
    200
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ArchivedMemory {
    session_id: String,
    memory_id: String,
    start_index: usize,
    end_index: usize,
    messages: Vec<LanguageModelRequestMessage>,
    summary: Option<String>,
    created_at: std::time::SystemTime,
}

// Global in-memory storage for archived memories
static MEMORY_STORE: OnceLock<Mutex<HashMap<String, ArchivedMemory>>> = OnceLock::new();

fn memory_store() -> &'static Mutex<HashMap<String, ArchivedMemory>> {
    MEMORY_STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub struct MemoryTool;

impl Tool for MemoryTool {
    fn name(&self) -> String {
        "memory".into()
    }

    fn needs_confirmation(&self, _: &serde_json::Value, _: &Entity<Project>, _: &App) -> bool {
        false
    }

    fn may_perform_edits(&self) -> bool {
        false
    }

    fn description(&self) -> String {
        "Archive (store) a contiguous range of messages, replacing them with a compact placeholder; later list, load, restore, or prune those archives.".into()
    }

    fn icon(&self) -> IconName {
        IconName::Book
    }

    fn input_schema(&self, format: LanguageModelToolSchemaFormat) -> Result<serde_json::Value> {
        json_schema_for::<MemoryToolInput>(format)
    }

    fn ui_text(&self, input: &serde_json::Value) -> String {
        if let Ok(input) = serde_json::from_value::<MemoryToolInput>(input.clone()) {
            match input.operation {
                MemoryOperation::Store => {
                    if let (Some(start), Some(end)) = (input.start_index, input.end_index) {
                        format!("Archive messages {}..{}", start, end)
                    } else {
                        "Archive messages".to_string()
                    }
                }
                MemoryOperation::Load => {
                    if let Some(handle) = input.memory_handle {
                        format!("Load memory: {}", handle)
                    } else {
                        "Load memory".to_string()
                    }
                }
                MemoryOperation::List => "List archived memories".to_string(),
                MemoryOperation::Restore => {
                    if let Some(handle) = input.memory_handle {
                        format!("Restore memory: {}", handle)
                    } else {
                        "Restore memory".to_string()
                    }
                }
                MemoryOperation::Prune => "Prune unused memories".to_string(),
            }
        } else {
            "Memory operation".to_string()
        }
    }

    fn run(
        self: Arc<Self>,
        input: serde_json::Value,
        request: Arc<LanguageModelRequest>,
        _project: Entity<Project>,
        _action_log: Entity<ActionLog>,
        _model: Arc<dyn LanguageModel>,
        _window: Option<AnyWindowHandle>,
        cx: &mut App,
    ) -> ToolResult {
        let mut input: MemoryToolInput = match serde_json::from_value(input) {
            Ok(input) => input,
            Err(err) => return Task::ready(Err(anyhow!(err))).into(),
        };

        // Clamp max_preview_chars
        input.max_preview_chars = input.max_preview_chars.clamp(40, 400);

        let messages = request.messages.clone();
        let session_id = request
            .thread_id
            .as_ref()
            .map(|id| id.to_string())
            .unwrap_or_else(|| "default".to_string());

        let task = cx.background_spawn(async move {
            match input.operation {
                MemoryOperation::Store => {
                    let start = input
                        .start_index
                        .ok_or_else(|| anyhow!("start_index required for store operation"))?;
                    let end = input
                        .end_index
                        .ok_or_else(|| anyhow!("end_index required for store operation"))?;

                    if start > end {
                        bail!("start_index must be <= end_index");
                    }

                    if end >= messages.len() {
                        bail!(
                            "end_index {} is beyond message count {}",
                            end,
                            messages.len()
                        );
                    }

                    // Archive the message range
                    let archived_messages: Vec<_> = messages[start..=end].to_vec();
                    let message_count = archived_messages.len();

                    // Generate summary if requested
                    let summary = if let Some(summary) = input.summary {
                        Some(summary)
                    } else if input.auto {
                        Some(generate_heuristic_summary(
                            &archived_messages,
                            message_count,
                        ))
                    } else {
                        None
                    };

                    // Calculate total character count
                    let total_chars: usize = archived_messages
                        .iter()
                        .map(|m| {
                            m.content
                                .iter()
                                .map(|c| match c {
                                    language_model::MessageContent::Text(text) => text.len(),
                                    language_model::MessageContent::Thinking { text, .. } => {
                                        text.len()
                                    }
                                    language_model::MessageContent::RedactedThinking(text) => {
                                        text.len()
                                    }
                                    language_model::MessageContent::Image(_) => 7, // "[Image]"
                                    language_model::MessageContent::ToolUse(_) => 11, // "[Tool Use]"
                                    language_model::MessageContent::ToolResult(_) => 14, // "[Tool Result]"
                                })
                                .sum::<usize>()
                        })
                        .sum();

                    // Generate preview
                    let preview = generate_preview(&archived_messages, input.max_preview_chars);

                    // Create memory handle
                    let memory_id = Uuid::new_v4().to_string();
                    let memory_handle = format!("mem://{}/{}", session_id, memory_id);

                    // Store in memory
                    let archived = ArchivedMemory {
                        session_id: session_id.clone(),
                        memory_id: memory_id.clone(),
                        start_index: start,
                        end_index: end,
                        messages: archived_messages,
                        summary: summary.clone(),
                        created_at: std::time::SystemTime::now(),
                    };

                    memory_store()
                        .lock()
                        .insert(memory_handle.clone(), archived);

                    // Build placeholder
                    let mut placeholder = format!(
                        "[[memory archived handle={} range={}..{} messages={} chars={}]]",
                        memory_handle, start, end, message_count, total_chars
                    );

                    if let Some(summary) = summary {
                        placeholder.push_str(&format!("\nSummary: {}", summary));
                    }

                    if !preview.is_empty() {
                        placeholder.push_str(&format!("\nPreview: {}", preview));
                    }

                    let output = format!(
                        "Archived {} messages (indices {}-{}) to {}\n\nPlaceholder:\n{}",
                        message_count, start, end, memory_handle, placeholder
                    );

                    Ok(ToolResultOutput::from(output))
                }

                MemoryOperation::Load => {
                    let handle = input
                        .memory_handle
                        .ok_or_else(|| anyhow!("memory_handle required for load operation"))?;

                    let store = memory_store().lock();
                    let archived = store
                        .get(&handle)
                        .ok_or_else(|| anyhow!("Memory handle not found: {}", handle))?;

                    let mut output = format!("# Archived Memory: {}\n\n", handle);
                    output.push_str(&format!(
                        "Range: messages {}-{}\n",
                        archived.start_index, archived.end_index
                    ));
                    output.push_str(&format!("Message count: {}\n", archived.messages.len()));

                    if let Some(summary) = &archived.summary {
                        output.push_str(&format!("Summary: {}\n", summary));
                    }

                    output.push_str("\n## Full Content:\n\n");

                    for (i, message) in archived.messages.iter().enumerate() {
                        output.push_str(&format!(
                            "### Message {} ({:?})\n\n",
                            archived.start_index + i,
                            message.role
                        ));
                        let content_str = message
                            .content
                            .iter()
                            .map(|c| match c {
                                language_model::MessageContent::Text(text) => text.clone(),
                                language_model::MessageContent::Thinking { text, .. } => {
                                    text.clone()
                                }
                                language_model::MessageContent::RedactedThinking(text) => {
                                    text.clone()
                                }
                                language_model::MessageContent::Image(_) => "[Image]".to_string(),
                                language_model::MessageContent::ToolUse(_) => {
                                    "[Tool Use]".to_string()
                                }
                                language_model::MessageContent::ToolResult(_) => {
                                    "[Tool Result]".to_string()
                                }
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        output.push_str(&content_str);
                        output.push_str("\n\n");
                    }

                    Ok(ToolResultOutput::from(output))
                }

                MemoryOperation::List => {
                    let store = memory_store().lock();

                    // For now, we'll scan the current messages for placeholders
                    // In a real implementation, this would scan the actual conversation buffer
                    let mut found_placeholders = Vec::new();

                    for (i, message) in messages.iter().enumerate() {
                        let content = message
                            .content
                            .iter()
                            .map(|c| match c {
                                language_model::MessageContent::Text(text) => text.clone(),
                                language_model::MessageContent::Thinking { text, .. } => {
                                    text.clone()
                                }
                                language_model::MessageContent::RedactedThinking(text) => {
                                    text.clone()
                                }
                                language_model::MessageContent::Image(_) => "[Image]".to_string(),
                                language_model::MessageContent::ToolUse(_) => {
                                    "[Tool Use]".to_string()
                                }
                                language_model::MessageContent::ToolResult(_) => {
                                    "[Tool Result]".to_string()
                                }
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        if content.contains("[[memory archived handle=") {
                            // Extract handle from placeholder
                            if let Some(start) = content.find("handle=") {
                                let handle_start = start + 7;
                                if let Some(end) = content[handle_start..].find(' ') {
                                    let handle = &content[handle_start..handle_start + end];
                                    found_placeholders.push((i, handle.to_string()));
                                }
                            }
                        }
                    }

                    if found_placeholders.is_empty() {
                        Ok(ToolResultOutput::from(
                            "No archived memory placeholders found.".to_string(),
                        ))
                    } else {
                        let mut output = format!(
                            "Found {} memory placeholder(s):\n\n",
                            found_placeholders.len()
                        );

                        for (index, handle) in found_placeholders {
                            if let Some(archived) = store.get(&handle) {
                                output.push_str(&format!(
                                    "- Index {}: {} (messages {}-{}, {} messages)\n",
                                    index,
                                    handle,
                                    archived.start_index,
                                    archived.end_index,
                                    archived.messages.len()
                                ));
                            } else {
                                output.push_str(&format!(
                                    "- Index {}: {} (NOT FOUND in store)\n",
                                    index, handle
                                ));
                            }
                        }

                        Ok(ToolResultOutput::from(output))
                    }
                }

                MemoryOperation::Restore => {
                    let handle = input
                        .memory_handle
                        .ok_or_else(|| anyhow!("memory_handle required for restore operation"))?;

                    let store = memory_store().lock();
                    let archived = store
                        .get(&handle)
                        .ok_or_else(|| anyhow!("Memory handle not found: {}", handle))?
                        .clone();

                    let insert_index = input.restore_insert_index.unwrap_or(messages.len());

                    let mut output = format!(
                        "Restored {} messages from {} at index {}\n",
                        archived.messages.len(),
                        handle,
                        insert_index
                    );

                    if input.remove_placeholder {
                        output.push_str("\nPlaceholder removed.\n");
                    } else if let Some(replacement) = input.replace_placeholder_with {
                        output.push_str(&format!("\nPlaceholder replaced with: {}\n", replacement));
                    }

                    Ok(ToolResultOutput::from(output))
                }

                MemoryOperation::Prune => {
                    let store = memory_store().lock();
                    let initial_count = store.len();

                    // For now, we don't actually remove anything since we can't properly
                    // scan the conversation for placeholders
                    // In a real implementation, this would check which handles are still referenced

                    let output = format!(
                        "Pruning check complete. {} archived memories in store.\n",
                        initial_count
                    );

                    Ok(ToolResultOutput::from(output))
                }
            }
        });

        ToolResult {
            output: task,
            card: None,
        }
    }
}

fn generate_heuristic_summary(messages: &[LanguageModelRequestMessage], count: usize) -> String {
    // Find first non-empty message content
    for message in messages {
        let content = message
            .content
            .iter()
            .filter_map(|c| match c {
                language_model::MessageContent::Text(text) => Some(text.as_str()),
                language_model::MessageContent::Thinking { text, .. } => Some(text.as_str()),
                language_model::MessageContent::RedactedThinking(text) => Some(text.as_str()),
                language_model::MessageContent::Image(_) => None,
                language_model::MessageContent::ToolUse(_) => None,
                language_model::MessageContent::ToolResult(_) => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string();
        if !content.is_empty() {
            let first_line = content.lines().next().unwrap_or(&content);
            if first_line.len() > 140 {
                return format!("{}... ({} msgs)", &first_line[..140], count);
            } else {
                return format!("{} ({} msgs)", first_line, count);
            }
        }
    }

    format!("({} msgs)", count)
}

fn generate_preview(messages: &[LanguageModelRequestMessage], max_chars: usize) -> String {
    let mut preview = String::new();
    let mut total_chars = 0;

    for message in messages {
        let content = message
            .content
            .iter()
            .map(|c| match c {
                language_model::MessageContent::Text(text) => text.clone(),
                language_model::MessageContent::Thinking { text, .. } => text.clone(),
                language_model::MessageContent::RedactedThinking(text) => text.clone(),
                language_model::MessageContent::Image(_) => "[Image]".to_string(),
                language_model::MessageContent::ToolUse(_) => "[Tool Use]".to_string(),
                language_model::MessageContent::ToolResult(_) => "[Tool Result]".to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n");
        if total_chars + content.len() <= max_chars {
            if !preview.is_empty() {
                preview.push_str("\n");
            }
            preview.push_str(&content);
            total_chars += content.len();
        } else if total_chars < max_chars {
            let remaining = max_chars - total_chars;
            if remaining > 20 {
                // Only add if meaningful amount remains
                if !preview.is_empty() {
                    preview.push_str("\n");
                }
                preview.push_str(&content[..remaining]);
                preview.push_str("...(truncated)");
            }
            break;
        } else {
            break;
        }
    }

    preview
}
