use crate::{AgentTool, ToolCallEventStream};
use agent_client_protocol::ToolKind;
use anyhow::Result;
use gpui::{App, SharedString, Task};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Lists conversation messages with stable indices and previews for context management.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ListHistoryToolInput {
    /// Inclusive starting message index (default 0)
    #[serde(default)]
    start: usize,

    /// Number of messages to enumerate (default 40, clamped 1..500)
    #[serde(default = "default_limit")]
    limit: usize,

    /// Preview character cap per message (default 160, clamped 16..4096)
    #[serde(default = "default_max_chars")]
    max_chars_per_message: usize,

    /// If true, appends full text of each listed message after the table
    #[serde(default)]
    include_full_markdown: bool,
}

fn default_limit() -> usize {
    40
}

fn default_max_chars() -> usize {
    160
}

use crate::thread::Thread;
use gpui::WeakEntity;

pub struct ListHistoryTool {
    thread: WeakEntity<Thread>,
}

impl ListHistoryTool {
    pub fn new(thread: WeakEntity<Thread>) -> Self {
        Self { thread }
    }
}

impl AgentTool for ListHistoryTool {
    type Input = ListHistoryToolInput;
    type Output = String;

    fn name() -> &'static str {
        "list_history"
    }

    fn kind() -> ToolKind {
        ToolKind::Read
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        if let Ok(input) = input {
            format!(
                "List history (start: {}, limit: {})",
                input.start, input.limit
            )
            .into()
        } else {
            "List conversation history".into()
        }
    }

    fn run(
        self: Arc<Self>,
        mut input: Self::Input,
        _event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output>> {
        input.limit = input.limit.clamp(1, 500);
        input.max_chars_per_message = input.max_chars_per_message.clamp(16, 4096);

        let Some(thread) = self.thread.upgrade() else {
            return Task::ready(Ok(String::from(
                "# Conversation History\n\nThread no longer exists.\n",
            )));
        };

        // Obtain full markdown representation of the thread and derive messages
        let full_markdown = thread.read_with(cx, |t, _| t.to_markdown());

        // Parse messages from markdown. Each message starts with a heading "## User" or "## Assistant".
        // Resume markers "[resume]" are treated as distinct messages with role "Resume".
        #[derive(Clone)]
        struct ParsedMessage {
            role: String,
            markdown: String,
        }

        let mut messages: Vec<ParsedMessage> = Vec::new();
        let mut current_role: Option<String> = None;
        let mut current_heading: Option<String> = None;
        let mut current_lines: Vec<String> = Vec::new();

        let mut push_current = |messages: &mut Vec<ParsedMessage>,
                                role: &mut Option<String>,
                                heading: &mut Option<String>,
                                lines: &mut Vec<String>| {
            if let (Some(r), Some(h)) = (role.take(), heading.take()) {
                let body = if lines.is_empty() {
                    format!("{h}\n")
                } else {
                    format!("{h}\n\n{}", lines.join("\n"))
                };
                messages.push(ParsedMessage {
                    role: r,
                    markdown: body,
                });
            }
            lines.clear();
        };

        for line in full_markdown.lines() {
            if line.starts_with("## User") || line.starts_with("## Assistant") {
                // New message boundary
                push_current(
                    &mut messages,
                    &mut current_role,
                    &mut current_heading,
                    &mut current_lines,
                );
                if line.starts_with("## User") {
                    current_role = Some("User".to_string());
                } else {
                    current_role = Some("Assistant".to_string());
                }
                current_heading = Some(line.to_string());
                continue;
            }
            if line.trim() == "[resume]" {
                // Flush any current message
                push_current(
                    &mut messages,
                    &mut current_role,
                    &mut current_heading,
                    &mut current_lines,
                );
                // Store resume as a standalone message
                messages.push(ParsedMessage {
                    role: "Resume".to_string(),
                    markdown: "[resume]\n".to_string(),
                });
                continue;
            }
            if current_role.is_some() {
                current_lines.push(line.to_string());
            }
        }
        // Push last accumulated
        push_current(
            &mut messages,
            &mut current_role,
            &mut current_heading,
            &mut current_lines,
        );

        let total_messages = messages.len();

        if input.start >= total_messages {
            let mut out = String::new();
            out.push_str("# Conversation History\n\n");
            out.push_str(&format!(
                "No messages found starting from index {} (total messages: {})\n",
                input.start, total_messages
            ));
            return Task::ready(Ok(out));
        }

        let end_index = (input.start + input.limit).min(total_messages);
        let slice = &messages[input.start..end_index];

        let mut output = String::new();
        output.push_str("# Conversation History\n\n");

        // Summary JSON block
        output.push_str("```json\n");
        output.push_str(
            &serde_json::to_string_pretty(&serde_json::json!({
                "total_messages": total_messages,
                "showing_range": format!("{}..{}", input.start, end_index),
                "messages_shown": end_index - input.start,
            }))
            .unwrap_or_default(),
        );
        output.push_str("\n```\n\n");

        // Table header
        output.push_str("| Idx | Role | Chars | Preview |\n");
        output.push_str("|-----|------|-------|---------|\n");

        for (offset, pm) in slice.iter().enumerate() {
            let idx = input.start + offset;
            let role = &pm.role;
            let full = pm.markdown.as_str();
            let chars = full.len();
            let preview = if full.len() <= input.max_chars_per_message {
                full
            } else {
                &full[..input.max_chars_per_message]
            };
            let mut preview = preview.replace('|', "\\|").replace('\n', " ");
            if chars > input.max_chars_per_message {
                preview.push_str("...");
            }
            output.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                idx, role, chars, preview
            ));
        }

        if input.include_full_markdown {
            output.push_str("\n## Full Message Content\n\n");
            for (offset, pm) in slice.iter().enumerate() {
                let idx = input.start + offset;
                output.push_str(&format!("### Message {} ({})\n\n", idx, pm.role));
                output.push_str(&pm.markdown);
                if !pm.markdown.ends_with('\n') {
                    output.push('\n');
                }
                output.push('\n');
            }
        }

        Task::ready(Ok(output))
    }
}
