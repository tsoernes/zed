use crate::thread::Message;
use crate::{AgentTool, Thread, ToolCallEventStream};
use agent_client_protocol as acp;
use anyhow::{Result, anyhow};
use gpui::{App, SharedString, Task, WeakEntity};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Input parameters for the list_history tool.
///
/// This tool enumerates a slice of the conversation history with stable indices that
/// the language model can use with `memory` or `rewrite_history`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ListHistoryToolInput {
    /// 0-based starting index (inclusive). Defaults to 0.
    start: Option<usize>,
    /// Maximum number of messages to list. Defaults to 40.
    limit: Option<usize>,
    /// Maximum characters per message preview (after trimming). Defaults to 160.
    max_chars_per_message: Option<usize>,
    /// If true, include a section after the table with each listed message's full (untruncated) markdown.
    include_full_markdown: Option<bool>,
}

/// Tool for enumerating thread messages with indices and previews to support
/// selective compression (memory / rewrite_history) before hitting token limits.
///
/// Each message is annotated upstream with an index (in the system->model request),
/// but this tool provides a structured digest and JSON you can parse reliably.
/// Use this before calling `memory` or `rewrite_history` to pick correct index ranges.
pub struct ListHistoryTool {
    thread: WeakEntity<Thread>,
}

impl ListHistoryTool {
    pub fn new(thread: WeakEntity<Thread>) -> Self {
        Self { thread }
    }

    fn message_kind(message: &Message, markdown: &str) -> &'static str {
        // Detect compression placeholders first
        if markdown.contains("[[memory:") {
            "compressed_memory"
        } else if markdown.contains("[[history_rewrite") {
            "compressed_rewrite"
        } else {
            match message {
                Message::User(_) => "user",
                Message::Agent(_) => "assistant",
                Message::Resume => "resume",
            }
        }
    }

    fn role_str(message: &Message) -> &'static str {
        match message {
            Message::User(_) | Message::Resume => "user",
            Message::Agent(_) => "assistant",
        }
    }

    fn preview(markdown: &str, max_chars: usize) -> String {
        let first_line = markdown
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim();

        if first_line.len() <= max_chars {
            first_line.to_string()
        } else {
            let mut truncated = first_line[..max_chars].to_string();
            truncated.push('…');
            truncated
        }
    }

    fn escape_table_cell(s: &str) -> String {
        // Basic escaping: replace pipe and newlines to keep table intact.
        s.replace('|', r"\|").replace('\n', " ")
    }
}

impl AgentTool for ListHistoryTool {
    type Input = ListHistoryToolInput;
    type Output = String;

    fn name() -> &'static str {
        "list_history"
    }

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Read
    }

    fn description(&self) -> SharedString {
        "Enumerate conversation message indices with previews to decide which range to compress using `memory` or `rewrite_history`. Provides JSON and a markdown table. Use this when planning context reduction.".into()
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        if let Ok(i) = input {
            let start = i.start.unwrap_or(0);
            let limit = i.limit.unwrap_or(40);
            format!("List history {start}..{}", start + limit.saturating_sub(1)).into()
        } else {
            "List history".into()
        }
    }

    fn run(
        self: Arc<Self>,
        input: Self::Input,
        event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<String>> {
        let Some(thread) = self.thread.upgrade() else {
            return Task::ready(Err(anyhow!("thread no longer exists")));
        };

        let start = input.start.unwrap_or(0);
        let limit = input.limit.unwrap_or(40).max(1).min(500);
        let max_chars = input.max_chars_per_message.unwrap_or(160).max(16).min(4096);
        let include_full = input.include_full_markdown.unwrap_or(false);

        let output = thread.read_with(cx, |thread, _| {
            let total = thread.message_len();
            if start >= total {
                return Ok(format!(
                    "No messages: start index {start} is >= total messages ({total})."
                ));
            }
            let end_inclusive = (start + limit - 1).min(total.saturating_sub(1));

            #[derive(Serialize)]
            struct MsgSummary {
                index: usize,
                role: String,
                kind: String,
                char_count: usize,
                preview: String,
            }

            // Collect owned previews directly inside summaries to avoid self-referential borrowing.
            let mut summaries: Vec<MsgSummary> = Vec::new();

            for (idx, message) in thread.iter_messages().enumerate() {
                if idx < start {
                    continue;
                }
                if idx > end_inclusive {
                    break;
                }
                let md = message.to_markdown();
                let char_count = md.len();
                let preview = Self::preview(&md, max_chars);
                let kind = Self::message_kind(message, &md).to_string();
                let role = Self::role_str(message).to_string();
                summaries.push(MsgSummary {
                    index: idx,
                    role,
                    kind,
                    char_count,
                    preview,
                });
            }

            #[derive(Serialize)]
            struct HistorySlice {
                total_messages: usize,
                range_start: usize,
                range_end: usize,
                messages: Vec<MsgSummary>,
            }

            let slice = HistorySlice {
                total_messages: total,
                range_start: start,
                range_end: end_inclusive,
                messages: summaries,
            };

            let json_block = serde_json::to_string_pretty(&slice)
                .unwrap_or_else(|_| "{\"error\":\"serialization\"}".into());

            // Markdown table
            let mut table = String::new();
            table.push_str("|Idx|Role|Kind|Chars|Preview|\n");
            table.push_str("|---|----|----|-----|--------|\n");
            for s in &summaries {
                let pv = Self::escape_table_cell(&s.preview);
                table.push_str(&format!(
                    "|{}|{}|{}|{}|{}|\n",
                    s.index, s.role, s.kind, s.char_count, pv
                ));
            }

            // Optional full markdown appendix
            let mut appendix = String::new();
            if include_full {
                for s in &summaries {
                    if let Some(message) = thread.iter_messages().nth(s.index) {
                        let full_md = message.to_markdown();
                        appendix.push_str(&format!(
                            "\n### Message {}\n\n```\n{}\n```\n",
                            s.index, full_md
                        ));
                    }
                }
            }

            let result = format!(
                "History indices {start}..{end_inclusive} of {total}\n\n\
                 JSON summary:\n```json\n{json_block}\n```\n\n\
                 Table:\n{table}\n\
                 Guidance: Use these indices with `memory` (to store & replace) or `rewrite_history` (to remove / summarize ranges). \
                 Retrieve archived content later via the memory handle.\n\
                 {}",
                if include_full { appendix } else { String::new() }
            );

            Ok(result)
        });

        match output {
            Ok(ref out) => {
                event_stream.update_fields(acp::ToolCallUpdateFields {
                    content: Some(vec![out.clone().into()]),
                    ..Default::default()
                });
            }
            Err(_) => {}
        }

        Task::ready(output)
    }
}
