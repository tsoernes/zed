use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::schema::json_schema_for;
use action_log::ActionLog;
use anyhow::{Result, anyhow};
use assistant_tool::{Tool, ToolResult, ToolResultOutput};
use gpui::{AnyWindowHandle, App, AppContext, Entity, Task};
use language_model::{
    LanguageModel, LanguageModelRequest, LanguageModelRequestMessage,
    LanguageModelToolSchemaFormat, MessageContent,
};
use project::Project;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ui::IconName;

/// Operation the memory tool should perform.
#[derive(Debug, Serialize, Deserialize, JsonSchema, Clone)]
#[serde(rename_all = "snake_case")]
pub enum MemoryOperation {
    /// List stored memory segments (optionally limited).
    List {
        #[serde(default)]
        limit: Option<usize>,
    },
    /// Show aggregate statistics.
    Stats,
    /// Store (archive) a contiguous range of messages [start, end) (end exclusive).
    Store {
        start: usize,
        end: usize,
        /// Optional explicit summary. If omitted a lightweight one is synthesized.
        #[serde(default)]
        summary: Option<String>,
    },
    /// Load (show) a specific stored memory segment by id.
    Load {
        id: u64,
        #[serde(default)]
        include_messages: bool,
    },
    /// Restore (replay) a stored memory segment by id as markdown (does not modify the thread).
    Restore { id: u64 },
    /// Prune (delete) a stored memory segment by id.
    Prune { id: u64 },
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct MemoryToolInput {
    pub operation: MemoryOperation,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct MemorySegment {
    id: u64,
    start: usize,
    end: usize,
    summary: String,
    message_char_count: usize,
    message_count: usize,
    stored_epoch_ms: u128,
    messages: Vec<String>,
}

impl MemorySegment {
    fn to_metadata_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "start": self.start,
            "end": self.end,
            "count": self.message_count,
            "chars": self.message_char_count,
            "summary": self.summary,
            "stored_epoch_ms": self.stored_epoch_ms,
        })
    }
}

#[derive(Default)]
struct MemoryStore {
    next_id: u64,
    segments: Vec<MemorySegment>,
}

impl MemoryStore {
    fn list(&self, limit: Option<usize>) -> Vec<MemorySegment> {
        let mut segs = self.segments.clone();
        segs.sort_by_key(|s| s.id);
        if let Some(l) = limit {
            segs.into_iter()
                .rev()
                .take(l)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect()
        } else {
            segs
        }
    }

    fn stats(&self) -> serde_json::Value {
        let total_segments = self.segments.len();
        let total_messages: usize = self.segments.iter().map(|s| s.message_count).sum();
        let total_chars: usize = self.segments.iter().map(|s| s.message_char_count).sum();
        serde_json::json!({
            "segments": total_segments,
            "messages": total_messages,
            "chars": total_chars
        })
    }

    fn insert(&mut self, segment: MemorySegment) -> u64 {
        let id = segment.id;
        self.segments.push(segment);
        id
    }

    fn prune(&mut self, id: u64) -> bool {
        let before = self.segments.len();
        self.segments.retain(|s| s.id != id);
        before != self.segments.len()
    }

    fn get(&self, id: u64) -> Option<MemorySegment> {
        self.segments.iter().find(|s| s.id == id).cloned()
    }

    fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        id
    }
}

static MEMORY_STORE: OnceLock<Mutex<MemoryStore>> = OnceLock::new();

fn store() -> &'static Mutex<MemoryStore> {
    MEMORY_STORE.get_or_init(|| Mutex::new(MemoryStore::default()))
}

pub struct MemoryTool;

impl Tool for MemoryTool {
    fn name(&self) -> String {
        "memory".into()
    }

    fn needs_confirmation(
        &self,
        input: &serde_json::Value,
        _project: &Entity<Project>,
        _cx: &App,
    ) -> bool {
        // Destructive operations: prune (and possibly future inline edits).
        if let Ok(parsed) = serde_json::from_value::<MemoryToolInput>(input.clone()) {
            matches!(parsed.operation, MemoryOperation::Prune { .. })
        } else {
            false
        }
    }

    fn may_perform_edits(&self) -> bool {
        // This implementation only inspects; does not mutate the thread content directly.
        false
    }

    fn description(&self) -> String {
        "Archive, list, inspect, and prune conversation memory segments for context management."
            .into()
    }

    fn icon(&self) -> IconName {
        // Chose Book as a generic document/memory representation; Archive variant does not exist.
        IconName::Book
    }

    fn input_schema(&self, format: LanguageModelToolSchemaFormat) -> Result<serde_json::Value> {
        json_schema_for::<MemoryToolInput>(format)
    }

    fn ui_text(&self, input: &serde_json::Value) -> String {
        if let Ok(parsed) = serde_json::from_value::<MemoryToolInput>(input.clone()) {
            match parsed.operation {
                MemoryOperation::List { .. } => "List stored memories".into(),
                MemoryOperation::Stats => "Memory stats".into(),
                MemoryOperation::Store { start, end, .. } => {
                    format!("Archive messages [{}..{})", start, end)
                }
                MemoryOperation::Load { id, .. } => format!("Load memory {}", id),
                MemoryOperation::Restore { id } => format!("Restore memory {}", id),
                MemoryOperation::Prune { id } => format!("Prune memory {}", id),
            }
        } else {
            "Memory operation".into()
        }
    }

    fn run(
        self: std::sync::Arc<Self>,
        input: serde_json::Value,
        request: std::sync::Arc<LanguageModelRequest>,
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

        let messages = request.messages.clone();

        let task = cx.background_spawn(async move {
            match handle_operation(parsed.operation, messages) {
                Ok(output) => Ok(ToolResultOutput::from(output)),
                Err(err) => Err(err),
            }
        });

        ToolResult {
            output: task,
            card: None,
        }
    }
}

fn handle_operation(
    op: MemoryOperation,
    messages: Vec<LanguageModelRequestMessage>,
) -> Result<String> {
    match op {
        MemoryOperation::List { limit } => {
            let guard = store()
                .lock()
                .map_err(|_| anyhow!("Memory store poisoned"))?;
            let segs = guard.list(limit);
            let mut out = String::new();
            out.push_str("# Stored Memories\n\n");
            out.push_str("```json\n");
            let list: Vec<_> = segs.iter().map(|s| s.to_metadata_json()).collect();
            out.push_str(&serde_json::to_string_pretty(&list)?);
            out.push('\n');
            out.push_str("```\n");
            Ok(out)
        }
        MemoryOperation::Stats => {
            let guard = store()
                .lock()
                .map_err(|_| anyhow!("Memory store poisoned"))?;
            let stats = guard.stats();
            let mut out = String::new();
            out.push_str("# Memory Stats\n\n```json\n");
            out.push_str(&serde_json::to_string_pretty(&stats)?);
            out.push('\n');
            out.push_str("```\n");
            Ok(out)
        }
        MemoryOperation::Store {
            start,
            end,
            summary,
        } => {
            if start >= end {
                return Err(anyhow!("start must be < end"));
            }
            if end > messages.len() {
                return Err(anyhow!(
                    "end ({}) exceeds message count ({})",
                    end,
                    messages.len()
                ));
            }

            let slice = &messages[start..end];
            let mut collected = Vec::with_capacity(slice.len());
            let mut char_total = 0usize;

            for m in slice {
                let text_parts: Vec<String> = m
                    .content
                    .iter()
                    .map(|c| match c {
                        MessageContent::Text(t) => t.clone(),
                        MessageContent::Thinking { text, .. } => text.clone(),
                        MessageContent::RedactedThinking(t) => t.clone(),
                        MessageContent::Image(_) => "[Image]".to_string(),
                        MessageContent::ToolUse(_) => "[Tool Use]".to_string(),
                        MessageContent::ToolResult(_) => "[Tool Result]".to_string(),
                    })
                    .collect();
                let joined = text_parts.join("\n");
                char_total += joined.len();
                collected.push(joined);
            }

            let synthesized = if let Some(s) = summary {
                s
            } else {
                synthesize_summary(&collected)
            };

            let stored_epoch_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or_default();

            let mut guard = store()
                .lock()
                .map_err(|_| anyhow!("Memory store poisoned"))?;
            let id = {
                let id = guard.next_id();
                let seg = MemorySegment {
                    id,
                    start,
                    end,
                    summary: synthesized,
                    message_char_count: char_total,
                    message_count: collected.len(),
                    stored_epoch_ms,
                    messages: collected,
                };
                guard.insert(seg)
            };

            let segment = guard
                .get(id)
                .ok_or_else(|| anyhow!("Failed to retrieve stored segment"))?;
            let mut out = String::new();
            out.push_str("# Stored Memory Segment\n\n");
            out.push_str("```json\n");
            out.push_str(&serde_json::to_string_pretty(&segment.to_metadata_json())?);
            out.push('\n');
            out.push_str("```\n");
            Ok(out)
        }
        MemoryOperation::Load {
            id,
            include_messages,
        } => {
            let guard = store()
                .lock()
                .map_err(|_| anyhow!("Memory store poisoned"))?;
            if let Some(seg) = guard.get(id) {
                let mut out = String::new();
                out.push_str("# Memory Segment\n\n```json\n");
                out.push_str(&serde_json::to_string_pretty(&seg.to_metadata_json())?);
                out.push('\n');
                out.push_str("```\n");
                if include_messages {
                    out.push_str("\n## Messages\n\n");
                    for (i, msg) in seg.messages.iter().enumerate() {
                        out.push_str(&format!(
                            "### [{}] Message {} (chars: {})\n\n",
                            seg.id,
                            i,
                            msg.len()
                        ));
                        out.push_str(msg);
                        out.push_str("\n\n");
                    }
                }
                Ok(out)
            } else {
                Err(anyhow!("No memory segment with id {}", id))
            }
        }
        MemoryOperation::Restore { id } => {
            let guard = store()
                .lock()
                .map_err(|_| anyhow!("Memory store poisoned"))?;
            if let Some(seg) = guard.get(id) {
                let mut out = String::new();
                out.push_str("# Restored Memory (Read-Only)\n\n");
                out.push_str(&format!(
                    "Segment {} (messages [{}..{}])\n\n",
                    seg.id, seg.start, seg.end
                ));
                out.push_str(&format!("**Summary:** {}\n\n", seg.summary));
                for (i, msg) in seg.messages.iter().enumerate() {
                    out.push_str(&format!("### Message {}\n\n", i));
                    out.push_str(msg);
                    out.push_str("\n\n");
                }
                Ok(out)
            } else {
                Err(anyhow!("No memory segment with id {}", id))
            }
        }
        MemoryOperation::Prune { id } => {
            let mut guard = store()
                .lock()
                .map_err(|_| anyhow!("Memory store poisoned"))?;
            if guard.prune(id) {
                Ok(format!(
                    "# Pruned Memory Segment\n\nRemoved segment {}\n",
                    id
                ))
            } else {
                Err(anyhow!("No memory segment with id {}", id))
            }
        }
    }
}

fn synthesize_summary(messages: &[String]) -> String {
    if messages.is_empty() {
        return "Empty message range".into();
    }
    let first = truncate(messages.first().unwrap(), 48);
    let last = truncate(messages.last().unwrap(), 48);
    if messages.len() == 1 {
        format!("Single message: {}", first)
    } else {
        format!(
            "{} msgs | first: {} | last: {}",
            messages.len(),
            first,
            last
        )
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut shortened = s.chars().take(max).collect::<String>();
        shortened.push('…');
        shortened
    }
}
