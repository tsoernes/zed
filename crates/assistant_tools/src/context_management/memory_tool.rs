use crate::context_management::memory_ops::GlobalMemoryBackend;
use crate::schema::json_schema_for;
use action_log::ActionLog;
use anyhow::{anyhow, Result};
use assistant_tool::{Tool, ToolResult};
use gpui::{AnyWindowHandle, App, Entity, Task};
use language_model::{LanguageModel, LanguageModelRequest, LanguageModelToolSchemaFormat};
use project::Project;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ui::IconName;

/// Operation the memory tool should perform.
/// Uses thread-backed archive (agent2 Thread) instead of an internal stub store.
#[derive(Debug, Serialize, JsonSchema, Clone)]
#[serde(rename_all = "snake_case")]
pub enum MemoryOperation {
    /// List stored memory segments (optionally limited).
    List {
        #[serde(default)]
        limit: Option<usize>,
    },
    /// Show aggregate statistics.
    Stats {},
    /// Store (archive) a contiguous range of messages [start, end) (end exclusive).
    Store {
        start: usize,
        end: usize,
        #[serde(default)]
        summary: Option<String>, // optional custom summary; if None or empty the thread auto-synthesizes and sanitizes
    },
    /// Load (show) a specific stored memory segment by id.
    Load {
        id: u64,
        #[serde(default)]
        include_messages: bool,
    },
    /// Restore a stored segment (reinsert original messages).
    Restore { id: u64 },
}

/// Helper struct for deserialization that uses the standard serde derive.
/// This avoids infinite recursion in the custom deserializer.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MemoryOperationHelper {
    List {
        #[serde(default)]
        limit: Option<usize>,
    },
    Stats {},
    Store {
        start: usize,
        end: usize,
        #[serde(default)]
        summary: Option<String>,
    },
    Load {
        id: u64,
        #[serde(default)]
        include_messages: bool,
    },
    Restore {
        id: u64,
    },
}

impl From<MemoryOperationHelper> for MemoryOperation {
    fn from(helper: MemoryOperationHelper) -> Self {
        match helper {
            MemoryOperationHelper::List { limit } => MemoryOperation::List { limit },
            MemoryOperationHelper::Stats {} => MemoryOperation::Stats {},
            MemoryOperationHelper::Store {
                start,
                end,
                summary,
            } => MemoryOperation::Store {
                start,
                end,
                summary,
            },
            MemoryOperationHelper::Load {
                id,
                include_messages,
            } => MemoryOperation::Load {
                id,
                include_messages,
            },
            MemoryOperationHelper::Restore { id } => MemoryOperation::Restore { id },
        }
    }
}

/// Custom deserializer to handle both direct JSON and string-wrapped JSON.
/// This works around an issue where MCP invocations wrap parameters as strings.
impl<'de> Deserialize<'de> for MemoryOperation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;
        let value = serde_json::Value::deserialize(deserializer)?;

        // Try direct deserialization first (normal case)
        match serde_json::from_value::<MemoryOperationHelper>(value.clone()) {
            Ok(helper) => return Ok(helper.into()),
            Err(_) => {
                // Handle string-wrapped JSON (MCP invocation case)
                if let serde_json::Value::String(ref s) = value {
                    match serde_json::from_str::<MemoryOperationHelper>(s) {
                        Ok(helper) => return Ok(helper.into()),
                        Err(e) => {
                            return Err(D::Error::custom(format!(
                                "Failed to parse string-wrapped MemoryOperation: {}",
                                e
                            )))
                        }
                    }
                }

                return Err(D::Error::custom(format!(
                    "Failed to deserialize MemoryOperation from: {:?}",
                    value
                )));
            }
        }
    }
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct MemoryToolInput {
    pub operation: MemoryOperation,
}

pub struct MemoryTool;

impl Tool for MemoryTool {
    fn name(&self) -> String {
        "memory".into()
    }

    fn needs_confirmation(
        &self,
        _input: &serde_json::Value,
        _project: &Entity<Project>,
        _cx: &App,
    ) -> bool {
        false
    }

    fn may_perform_edits(&self) -> bool {
        // Store / Restore mutate thread state (messages) via the backend.
        true
    }

    fn description(&self) -> String {
        "Archive, list, inspect, and restore conversation memory segments (thread-backed).".into()
    }

    fn icon(&self) -> IconName {
        IconName::Book
    }

    fn input_schema(&self, format: LanguageModelToolSchemaFormat) -> Result<serde_json::Value> {
        let schema = json_schema_for::<MemoryToolInput>(format);
        match &schema {
            Ok(_) => log::info!(
                "MemoryTool input_schema generated successfully (format={:?})",
                format
            ),
            Err(e) => log::error!(
                "MemoryTool input_schema generation failed (format={:?}): {}",
                format,
                e
            ),
        }
        schema
    }

    fn ui_text(&self, input: &serde_json::Value) -> String {
        if let Ok(parsed) = serde_json::from_value::<MemoryToolInput>(input.clone()) {
            match parsed.operation {
                MemoryOperation::List { .. } => "List stored memories".into(),
                MemoryOperation::Stats {} => "Memory stats".into(),
                MemoryOperation::Store { start, end, .. } => {
                    format!("Archive messages [{}..{})", start, end)
                }
                MemoryOperation::Load { id, .. } => format!("Load memory {}", id),
                MemoryOperation::Restore { id } => format!("Restore memory {}", id),
            }
        } else {
            "Memory operation".into()
        }
    }

    fn run(
        self: std::sync::Arc<Self>,
        input: serde_json::Value,
        _request: std::sync::Arc<LanguageModelRequest>,
        _project: Entity<Project>,
        _action_log: Entity<ActionLog>,
        _model: std::sync::Arc<dyn LanguageModel>,
        _window: Option<AnyWindowHandle>,
        cx: &mut App,
    ) -> ToolResult {
        let parsed: MemoryToolInput = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => {
                return ToolResult {
                    output: Task::ready(Err(anyhow!("Invalid input: {}", e))),
                    card: None,
                };
            }
        };
        // Obtain backend (thread-backed if registered, otherwise noop).
        let backend = GlobalMemoryBackend::get(cx);

        // Acquire active thread and perform operations only when agent2 is available.
        // Provide a graceful fallback when agent2 is not compiled in.
        // Execute via backend (thread-backed if registered).
        let op = parsed.operation.clone();
        // Execute synchronously so backend receives &mut App (not &mut AsyncApp) references.
        let task = {
            let result: Result<_> = (|| match op {
                MemoryOperation::Store {
                    start,
                    end,
                    summary,
                } => {
                    // Pass mutable App reference for mutation
                    let meta = backend.store(cx, start, end, summary)?;
                    let json = serde_json::json!({
                        "id": meta.id,
                        "start": meta.start,
                        "end": meta.end,
                        "count": meta.count,
                        "chars": meta.chars,
                        "placeholder_chars": meta.placeholder_chars,
                        "token_savings_estimate": meta.token_savings_estimate,
                        "summary": meta.summary,
                        "stored_epoch_ms": meta.stored_epoch_ms
                    });
                    let mut md = String::new();
                    md.push_str("# Stored Memory Segment\n\n```json\n");
                    md.push_str(&serde_json::to_string_pretty(&json)?);
                    md.push_str("\n```\n");
                    Ok(md.into())
                }
                MemoryOperation::List { limit } => {
                    let metas = backend.list(cx, limit)?;
                    let list_json: Vec<_> = metas
                        .iter()
                        .map(|m| {
                            serde_json::json!({
                                "id": m.id,
                                "start": m.start,
                                "end": m.end,
                                "count": m.count,
                                "chars": m.chars,
                                "placeholder_chars": m.placeholder_chars,
                                "token_savings_estimate": m.token_savings_estimate,
                                "summary": m.summary,
                                "stored_epoch_ms": m.stored_epoch_ms
                            })
                        })
                        .collect();
                    let mut md = String::new();
                    md.push_str("# Stored Memories\n\n```json\n");
                    md.push_str(&serde_json::to_string_pretty(&list_json)?);
                    md.push_str("\n```\n");
                    Ok(md.into())
                }
                MemoryOperation::Stats {} => {
                    let stats = backend.stats(cx)?;
                    let json = serde_json::json!({
                        "segments": stats.segments,
                        "messages": stats.messages,
                        "chars": stats.chars
                    });
                    let mut md = String::new();
                    md.push_str("# Memory Stats\n\n```json\n");
                    md.push_str(&serde_json::to_string_pretty(&json)?);
                    md.push_str("\n```\n");
                    Ok(md.into())
                }
                MemoryOperation::Load {
                    id,
                    include_messages,
                } => {
                    let detail = backend.load(cx, id, include_messages)?;
                    let meta = detail.meta;
                    let meta_json = serde_json::json!({
                        "id": meta.id,
                        "start": meta.start,
                        "end": meta.end,
                        "count": meta.count,
                        "chars": meta.chars,
                        "placeholder_chars": meta.placeholder_chars,
                        "token_savings_estimate": meta.token_savings_estimate,
                        "summary": meta.summary,
                        "stored_epoch_ms": meta.stored_epoch_ms
                    });
                    let mut md = String::new();
                    md.push_str("# Memory Segment\n\n```json\n");
                    md.push_str(&serde_json::to_string_pretty(&meta_json)?);
                    md.push_str("\n```\n");
                    if include_messages {
                        md.push_str("\n## Messages\n\n");
                        for (i, msg) in detail.messages.iter().enumerate() {
                            md.push_str(&format!("### [{}] Message {}\n\n{}\n\n", id, i, msg));
                        }
                    }
                    Ok(md.into())
                }
                MemoryOperation::Restore { id } => {
                    // Mutating restore requires &mut App after trait refactor.
                    // backend.restore consumes the mutable App to reinsert original messages.
                    let detail = backend.restore(cx, id)?;
                    let meta = detail.meta.clone();
                    let meta_json = serde_json::json!({
                        "id": meta.id,
                        "start": meta.start,
                        "end": meta.end,
                        "count": meta.count,
                        "chars": meta.chars,
                        "placeholder_chars": meta.placeholder_chars,
                        "token_savings_estimate": meta.token_savings_estimate,
                        "summary": meta.summary,
                        "stored_epoch_ms": meta.stored_epoch_ms
                    });
                    let mut md = String::new();
                    md.push_str("# Restored Memory Segment\n\n```json\n");
                    md.push_str(&serde_json::to_string_pretty(&meta_json)?);
                    md.push_str("\n```\n\n## Messages\n\n");
                    for (i, msg) in detail.messages.iter().enumerate() {
                        md.push_str(&format!("### Message {}\n\n{}\n\n", i, msg));
                    }
                    Ok(md.into())
                }
            })();
            Task::ready(result)
        };
        ToolResult {
            output: task,
            card: None,
        }
    }
}
