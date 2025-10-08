use crate::{AgentTool, ToolCallEventStream, thread::Thread};
use agent_client_protocol::ToolKind;
use anyhow::{Context as _, Result, anyhow};
use chrono::{DateTime, Utc};
use gpui::{App, Entity, SharedString, Task, WeakEntity};
use language_model::LanguageModelToolUse;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    path::PathBuf,
    sync::Arc,
};
use uuid::Uuid;
use zstd::stream::{read::Decoder as ZstdDecoder, write::Encoder as ZstdEncoder};

//
// Durable, file-based memory archiving backend.
//
// This implementation archives contiguous ranges of conversation messages
// to disk, replacing them in the active thread with a lightweight
// placeholder that contains a summary and a handle reference.
//
// Constraints / Limitations:
// * User messages: only plain text segments are archived; mentions and images are rejected.
// * Agent messages: text, thinking, redacted thinking, and tool uses are serialized (tool
//   result payloads are not preserved; only tool use metadata and raw_input are stored).
// * Persistence uses a per-thread directory tree under data_dir()/thread_archives.
// * Single successful restore is enforced once the placeholder has been removed.
// * Placeholder is retained by default unless the user sets remove_placeholder.
// * If persistence fails after extraction, the operation reverts: removes the placeholder
//   (if present) and reinserts the original messages at the original index.
//
// Future enhancements (not implemented here):
// * Persist tool result outputs.
// * Streaming / partial restore.
// * Archive rotation & size quotas.
//
// Error handling follows repository guidelines: no panics, propagate errors.
//

// ============================================================================
// Public Input/Output Structures
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryOperation {
    Store,
    Load,
    List,
    Restore,
    Prune,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct MemoryToolInput {
    operation: MemoryOperation,

    #[serde(skip_serializing_if = "Option::is_none")]
    start_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    end_index: Option<usize>,

    #[serde(skip_serializing_if = "Option::is_none")]
    memory_handle: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,

    #[serde(default)]
    auto: bool,

    #[serde(default = "default_preview_chars")]
    max_preview_chars: usize,

    #[serde(skip_serializing_if = "Option::is_none")]
    restore_insert_index: Option<usize>,

    #[serde(default)]
    remove_placeholder: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    replace_placeholder_with: Option<String>,
}

fn default_preview_chars() -> usize {
    160
}

// ============================================================================
// Internal Archive Representation
// ============================================================================

const ARCHIVE_ROOT_DIR: &str = "thread_archives";
const METADATA_FILE: &str = "metadata.json";
const MESSAGES_FILE: &str = "messages.zst";
const PLACEHOLDER_PREFIX: &str = "[[ARCHIVE handle=";

#[derive(Debug, Serialize, Deserialize)]
struct ArchiveMetadata {
    handle: String,
    thread_id: String,
    created_at: DateTime<Utc>,
    message_count: usize,
    summary: String,
    start_index: usize,
    end_index: usize,
    restored: bool,
    placeholder_removed: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredToolUse {
    id: String,
    name: String,
    raw_input: String,
    is_error: bool,
    output_text: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
enum StoredMessage {
    User {
        content: Vec<String>,
    },
    AgentRich {
        text_segments: Vec<String>,
        thinking_segments: Vec<(String, Option<String>)>,
        redacted_thinking: Vec<String>,
        tool_uses: Vec<StoredToolUse>,
    },
}

// ============================================================================
// MemoryTool Definition
// ============================================================================

pub struct MemoryTool {
    thread: WeakEntity<Thread>,
}

impl MemoryTool {
    pub fn new(thread: WeakEntity<Thread>) -> Self {
        Self { thread }
    }

    // Entry point dispatch
    fn execute(self: Arc<Self>, mut input: MemoryToolInput, cx: &mut App) -> Result<String> {
        input.max_preview_chars = input.max_preview_chars.clamp(16, 4096);
        match input.operation {
            MemoryOperation::Store => self.store(input, cx),
            MemoryOperation::Load => self.load(input, cx),
            MemoryOperation::List => self.list_archives(cx),
            MemoryOperation::Restore => self.restore(input, cx),
            MemoryOperation::Prune => self.prune(cx),
        }
    }

    // Obtain live thread or fail
    fn thread_entity(&self) -> Result<Entity<Thread>> {
        self.thread
            .upgrade()
            .ok_or_else(|| anyhow!("thread no longer exists"))
    }

    fn thread_id(&self, thread: &Thread) -> String {
        // Clone Arc<str> into owned String
        thread.id().0.to_string()
    }

    fn archive_dir(thread_id: &str, handle: &str) -> PathBuf {
        paths::data_dir()
            .join(ARCHIVE_ROOT_DIR)
            .join(thread_id)
            .join(handle)
    }

    fn thread_archive_root(thread_id: &str) -> PathBuf {
        paths::data_dir().join(ARCHIVE_ROOT_DIR).join(thread_id)
    }

    fn write_archive(&self, meta: &ArchiveMetadata, messages: &[StoredMessage]) -> Result<()> {
        let dir = Self::archive_dir(&meta.thread_id, &meta.handle);
        fs::create_dir_all(&dir)
            .with_context(|| format!("failed creating archive dir {:?}", dir))?;

        // metadata
        let metadata_path = dir.join(METADATA_FILE);
        let metadata_json = serde_json::to_vec_pretty(meta).context("serialize metadata")?;
        fs::write(&metadata_path, metadata_json)
            .with_context(|| format!("write {:?}", metadata_path))?;

        // messages (compressed)
        let messages_path = dir.join(MESSAGES_FILE);
        let file = fs::File::create(&messages_path)
            .with_context(|| format!("create {:?}", messages_path))?;
        let mut encoder = ZstdEncoder::new(file, 3).context("create zstd encoder")?;
        let payload = serde_json::to_vec(messages).context("serialize stored messages")?;
        encoder
            .write_all(&payload)
            .context("write compressed messages")?;
        encoder.finish().context("finish compression")?;
        Ok(())
    }

    fn read_archive_metadata(&self, thread_id: &str, handle: &str) -> Result<ArchiveMetadata> {
        let path = Self::archive_dir(thread_id, handle).join(METADATA_FILE);
        let data = fs::read(&path).with_context(|| format!("read {:?}", path))?;
        Ok(serde_json::from_slice(&data).context("parse metadata json")?)
    }

    fn write_archive_metadata(&self, meta: &ArchiveMetadata) -> Result<()> {
        let dir = Self::archive_dir(&meta.thread_id, &meta.handle);
        let path = dir.join(METADATA_FILE);
        let data = serde_json::to_vec_pretty(meta).context("serialize metadata")?;
        fs::write(&path, data).with_context(|| format!("write {:?}", path))?;
        Ok(())
    }

    fn read_archive_messages(&self, thread_id: &str, handle: &str) -> Result<Vec<StoredMessage>> {
        let path = Self::archive_dir(thread_id, handle).join(MESSAGES_FILE);
        let file = fs::File::open(&path).with_context(|| format!("open {:?}", path))?;
        let mut decoder = ZstdDecoder::new(file).context("create zstd decoder")?;
        let mut buf = Vec::new();
        decoder
            .read_to_end(&mut buf)
            .context("read compressed messages")?;
        Ok(serde_json::from_slice(&buf).context("parse messages json")?)
    }

    fn placeholder_text(meta: &ArchiveMetadata) -> String {
        // Multi-line placeholder: first line machine-parsable
        format!(
            "{prefix}{handle} count={count} created={created}]]\n{summary}",
            prefix = PLACEHOLDER_PREFIX,
            handle = meta.handle,
            count = meta.message_count,
            created = meta.created_at.to_rfc3339(),
            summary = meta.summary
        )
    }

    fn build_summary(
        &self,
        messages: &[StoredMessage],
        explicit: Option<&String>,
        auto: bool,
        max_preview_chars: usize,
    ) -> String {
        if let Some(s) = explicit {
            return s.clone();
        }
        if !auto {
            return "No summary provided".into();
        }
        let mut s = String::new();
        let mut remain = max_preview_chars;
        for (i, m) in messages.iter().enumerate() {
            if remain == 0 {
                break;
            }
            let excerpt = match m {
                StoredMessage::User { content } => content.join(" "),
                StoredMessage::AgentRich {
                    text_segments,
                    thinking_segments,
                    ..
                } => {
                    if !text_segments.is_empty() {
                        text_segments.join(" ")
                    } else if let Some((t, _)) = thinking_segments.first() {
                        t.clone()
                    } else {
                        String::from("<agent message>")
                    }
                }
            };
            let mut trimmed = excerpt.replace('\n', " ");
            if trimmed.len() > 64 {
                trimmed.truncate(64);
                trimmed.push('…');
            }
            let seg = format!("[{}] {}", i + 1, trimmed);
            let seg_len = seg.len();
            if seg_len > remain {
                break;
            }
            if !s.is_empty() {
                s.push(' ');
            }
            s.push_str(&seg);
            remain = remain.saturating_sub(seg_len);
        }
        if s.is_empty() {
            "Archived messages".into()
        } else {
            format!("Archived {} messages: {}", messages.len(), s)
        }
    }

    // Convert current thread messages into archive representation.
    fn to_stored_messages(&self, slice: &[crate::Message]) -> Result<Vec<StoredMessage>> {
        use crate::{AgentMessageContent, Message, UserMessageContent};
        let mut out = Vec::with_capacity(slice.len());
        for msg in slice {
            match msg {
                Message::User(user) => {
                    let mut texts = Vec::new();
                    for seg in &user.content {
                        match seg {
                            UserMessageContent::Text(t) => texts.push(t.clone()),
                            UserMessageContent::Mention { .. } | UserMessageContent::Image(_) => {
                                return Err(anyhow!(
                                    "archive range contains unsupported user content (mention/image)"
                                ));
                            }
                        }
                    }
                    out.push(StoredMessage::User { content: texts });
                }
                Message::Agent(agent) => {
                    let mut text_segments = Vec::new();
                    let mut thinking_segments = Vec::new();
                    let mut redacted_thinking = Vec::new();
                    let mut tool_uses = Vec::new();

                    for seg in &agent.content {
                        match seg {
                            AgentMessageContent::Text(t) => text_segments.push(t.clone()),
                            AgentMessageContent::Thinking { text, signature } => {
                                thinking_segments.push((text.clone(), signature.clone()));
                            }
                            AgentMessageContent::RedactedThinking(data) => {
                                redacted_thinking.push(data.clone());
                            }
                            AgentMessageContent::ToolUse(tool_use) => {
                                tool_uses.push(StoredToolUse {
                                    id: tool_use.id.to_string(),
                                    name: tool_use.name.to_string(),
                                    raw_input: tool_use.raw_input.clone(),
                                    is_error: false,
                                    output_text: None,
                                });
                            }
                        }
                    }

                    out.push(StoredMessage::AgentRich {
                        text_segments,
                        thinking_segments,
                        redacted_thinking,
                        tool_uses,
                    });
                }
                Message::Resume => {
                    return Err(anyhow!("cannot archive 'Resume' synthetic message variant"));
                }
            }
        }
        Ok(out)
    }

    fn reconstruct_messages(&self, stored: Vec<StoredMessage>) -> Vec<crate::Message> {
        use crate::{AgentMessage, AgentMessageContent, Message, UserMessage, UserMessageContent};
        stored
            .into_iter()
            .map(|sm| match sm {
                StoredMessage::User { content } => Message::User(UserMessage {
                    id: acp_thread::UserMessageId::new(),
                    content: content.into_iter().map(UserMessageContent::Text).collect(),
                }),
                StoredMessage::AgentRich {
                    text_segments,
                    thinking_segments,
                    redacted_thinking,
                    tool_uses,
                } => {
                    let mut content = Vec::new();
                    for t in text_segments {
                        content.push(AgentMessageContent::Text(t));
                    }
                    for (text, signature) in thinking_segments {
                        content.push(AgentMessageContent::Thinking { text, signature });
                    }
                    for data in redacted_thinking {
                        content.push(AgentMessageContent::RedactedThinking(data));
                    }
                    for tu in tool_uses {
                        let parsed = serde_json::from_str(&tu.raw_input)
                            .unwrap_or(serde_json::Value::String(tu.raw_input.clone()));
                        content.push(AgentMessageContent::ToolUse(LanguageModelToolUse {
                            id: tu.id.clone().into(),
                            name: tu.name.clone().into(),
                            raw_input: tu.raw_input,
                            input: parsed,
                            is_input_complete: true,
                        }));
                    }
                    Message::Agent(AgentMessage {
                        content,
                        tool_results: Default::default(),
                    })
                }
            })
            .collect()
    }

    // Store logic with revert-on-failure semantics.
    fn store(&self, input: MemoryToolInput, cx: &mut App) -> Result<String> {
        let start = input
            .start_index
            .ok_or_else(|| anyhow!("store requires start_index"))?;
        let end = input
            .end_index
            .ok_or_else(|| anyhow!("store requires end_index"))?;
        if end < start {
            return Err(anyhow!(
                "end_index ({end}) must be >= start_index ({start})"
            ));
        }

        let thread_ent = self.thread_entity()?;
        let thread_id = thread_ent.read(cx).id().0.to_string();
        let total = thread_ent.read(cx).messages().len();
        if start >= total || end >= total {
            return Err(anyhow!(
                "range {}..={} out of bounds (messages={})",
                start,
                end,
                total
            ));
        }

        let slice_vec = {
            let t = thread_ent.read(cx);
            t.messages()[start..=end].to_vec()
        };
        let stored_messages = self.to_stored_messages(&slice_vec)?;
        let summary = self.build_summary(
            &stored_messages,
            input.summary.as_ref(),
            input.auto,
            input.max_preview_chars,
        );

        let handle = Uuid::new_v4().to_string();
        let metadata = ArchiveMetadata {
            handle: handle.clone(),
            thread_id: thread_id.clone(),
            created_at: Utc::now(),
            message_count: stored_messages.len(),
            summary,
            start_index: start,
            end_index: end,
            restored: false,
            placeholder_removed: false,
        };

        let removed_messages = thread_ent.update(cx, |thread, thread_cx| {
            let removed = thread.extract_messages(start..=end, thread_cx)?;
            let placeholder_text = Self::placeholder_text(&metadata);
            thread.set_placeholder(start, placeholder_text, thread_cx)?;
            Ok::<_, anyhow::Error>(removed)
        })?;

        if let Err(persist_err) = self.write_archive(&metadata, &stored_messages) {
            let _ = thread_ent.update(cx, |thread, thread_cx| {
                if start < thread.messages().len() {
                    if let Some(idx) = Self::find_placeholder_index(thread, &handle) {
                        let _ = thread.remove_message(idx, thread_cx);
                    }
                }
                let _ = thread.insert_messages(start, removed_messages.clone(), thread_cx);
                Ok::<_, anyhow::Error>(())
            });
            return Err(anyhow!("failed to persist archive: {persist_err}"));
        }

        let mut out = String::new();
        use std::fmt::Write as _;
        writeln!(out, "# Memory Archive (Store)\n")?;
        writeln!(out, "Handle: {handle}")?;
        writeln!(out, "Range: {start}..={end}")?;
        writeln!(out, "Thread: {thread_id}")?;
        writeln!(out, "Messages: {}", metadata.message_count)?;
        Ok(out)
    }

    fn load(&self, input: MemoryToolInput, cx: &mut App) -> Result<String> {
        let handle = input
            .memory_handle
            .as_ref()
            .ok_or_else(|| anyhow!("load requires memory_handle"))?;
        let thread_id = self.thread_entity()?.read(cx).id().0.to_string();
        let meta = self.read_archive_metadata(&thread_id, handle)?;
        let stored = self.read_archive_messages(&thread_id, handle)?;
        let mut out = String::new();
        use std::fmt::Write as _;
        writeln!(out, "# Memory Load\n")?;
        writeln!(out, "Handle: {}", meta.handle)?;
        writeln!(out, "Restored: {}", meta.restored)?;
        writeln!(out, "Messages: {}", meta.message_count)?;
        writeln!(out, "Summary: {}", meta.summary)?;
        writeln!(out, "\n## Excerpt\n")?;
        let mut remaining = input.max_preview_chars.saturating_mul(50);
        for (i, sm) in stored.iter().enumerate() {
            if remaining == 0 {
                writeln!(out, "... truncated ...")?;
                break;
            }
            let joined = match sm {
                StoredMessage::User { content } => content.join(" "),
                StoredMessage::AgentRich {
                    text_segments,
                    thinking_segments,
                    ..
                } => {
                    if !text_segments.is_empty() {
                        text_segments.join(" ")
                    } else if let Some((t, _)) = thinking_segments.first() {
                        t.clone()
                    } else {
                        "<agent message>".into()
                    }
                }
            };
            let take = joined.len().min(remaining);
            writeln!(out, "### [{}]\n{}", i, &joined[..take])?;
            remaining = remaining.saturating_sub(take);
        }
        Ok(out)
    }

    fn list_archives(&self, cx: &mut App) -> Result<String> {
        let thread_ent = self.thread_entity()?;
        let thread_id = thread_ent.read(cx).id().0.to_string();
        let root = Self::thread_archive_root(&thread_id);
        let mut out = String::new();
        use std::fmt::Write as _;
        writeln!(out, "# Archived Memories\n")?;
        if !root.exists() {
            writeln!(out, "No archives.")?;
            return Ok(out);
        }
        let mut rows = Vec::new();
        for entry in fs::read_dir(&root).with_context(|| format!("scan {:?}", root))? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let handle = entry.file_name().to_string_lossy().to_string();
            let meta_path = entry.path().join(METADATA_FILE);
            if !meta_path.exists() {
                continue;
            }
            let data = fs::read(&meta_path)?;
            if let Ok(meta) = serde_json::from_slice::<ArchiveMetadata>(&data) {
                rows.push(meta);
            }
        }
        if rows.is_empty() {
            writeln!(out, "No archives.")?;
            return Ok(out);
        }

        let live_messages = thread_ent.read(cx).messages().to_vec();
        writeln!(
            out,
            "| Handle | Count | Range | Restored | Placeholder | Created | Summary |"
        )?;
        writeln!(
            out,
            "|--------|-------|-------|----------|-------------|---------|---------|"
        )?;
        for meta in rows {
            let placeholder_present =
                Self::find_placeholder_by_scan(&live_messages, &meta.handle).is_some();
            writeln!(
                out,
                "| {} | {} | {}..={} | {} | {} | {} | {} |",
                meta.handle,
                meta.message_count,
                meta.start_index,
                meta.end_index,
                if meta.restored { "yes" } else { "no" },
                if placeholder_present { "yes" } else { "no" },
                meta.created_at.to_rfc3339(),
                meta.summary
                    .replace('|', "\\|")
                    .replace('\n', " ")
                    .chars()
                    .take(64)
                    .collect::<String>()
            )?;
        }
        Ok(out)
    }

    fn restore(&self, input: MemoryToolInput, cx: &mut App) -> Result<String> {
        let handle = input
            .memory_handle
            .as_ref()
            .ok_or_else(|| anyhow!("restore requires memory_handle"))?;
        let thread_ent = self.thread_entity()?;
        let thread_id = thread_ent.read(cx).id().0.to_string();
        let mut meta = self.read_archive_metadata(&thread_id, handle)?;
        if meta.restored {
            let placeholder_index =
                thread_ent
                    .read(cx)
                    .messages()
                    .iter()
                    .enumerate()
                    .find_map(|(i, m)| {
                        if Self::message_has_placeholder(m, handle) {
                            Some(i)
                        } else {
                            None
                        }
                    });
            if placeholder_index.is_none() {
                return Err(anyhow!(
                    "archive {} already restored and placeholder removed",
                    handle
                ));
            }
        }

        let stored = self.read_archive_messages(&thread_id, handle)?;
        let reconstructed = self.reconstruct_messages(stored);

        let placeholder_index = {
            let t = thread_ent.read(cx);
            t.messages().iter().enumerate().find_map(|(i, m)| {
                if Self::message_has_placeholder(m, handle) {
                    Some(i)
                } else {
                    None
                }
            })
        };
        let target_index = if let Some(ix) = input.restore_insert_index {
            ix
        } else if let Some(ix) = placeholder_index {
            ix
        } else {
            thread_ent.read(cx).messages().len()
        };

        thread_ent.update(cx, |thread, thread_cx| {
            thread.insert_messages(target_index, reconstructed, thread_cx)?;
            if let Some(ph_ix) = placeholder_index {
                if input.remove_placeholder {
                    thread.remove_message(ph_ix, thread_cx)?;
                    meta.placeholder_removed = true;
                } else if let Some(repl) = &input.replace_placeholder_with {
                    thread.set_placeholder(ph_ix, repl.clone(), thread_cx)?;
                }
            }
            Ok::<_, anyhow::Error>(())
        })?;

        meta.restored = true;
        self.write_archive_metadata(&meta)?;

        let mut out = String::new();
        use std::fmt::Write as _;
        writeln!(out, "# Memory Restore\n")?;
        writeln!(out, "Handle: {}", meta.handle)?;
        writeln!(out, "Inserted messages: {}", meta.message_count)?;
        writeln!(out, "Insert index: {}", target_index)?;
        writeln!(
            out,
            "Placeholder: {}",
            if input.remove_placeholder {
                "removed"
            } else if input.replace_placeholder_with.is_some() {
                "replaced"
            } else {
                "retained"
            }
        )?;
        Ok(out)
    }

    fn prune(&self, cx: &mut App) -> Result<String> {
        let thread_ent = self.thread_entity()?;
        let thread_id = thread_ent.read(cx).id().0.to_string();
        let root = Self::thread_archive_root(&thread_id);
        let mut removed = Vec::new();
        if root.exists() {
            let live_messages = thread_ent.read(cx).messages().to_vec();
            for entry in fs::read_dir(&root)? {
                let entry = entry?;
                if !entry.file_type()?.is_dir() {
                    continue;
                }
                let handle = entry.file_name().to_string_lossy().to_string();
                let meta_path = entry.path().join(METADATA_FILE);
                if !meta_path.exists() {
                    continue;
                }
                let data = fs::read(&meta_path)?;
                let meta: ArchiveMetadata = match serde_json::from_slice(&data) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                if meta.restored {
                    let still_present =
                        Self::find_placeholder_by_scan(&live_messages, &handle).is_some();
                    if !still_present {
                        if fs::remove_dir_all(entry.path()).is_ok() {
                            removed.push(handle);
                        }
                    }
                }
            }
        }
        let mut out = String::new();
        use std::fmt::Write as _;
        writeln!(out, "# Memory Prune\n")?;
        if removed.is_empty() {
            writeln!(out, "No archives pruned.")?;
        } else {
            writeln!(out, "Pruned: {}", removed.join(", "))?;
        }
        Ok(out)
    }

    // Helpers for placeholder scanning
    fn message_has_placeholder(message: &crate::Message, handle: &str) -> bool {
        use crate::{AgentMessageContent, Message};
        match message {
            Message::Agent(agent) => agent.content.iter().any(|c| match c {
                AgentMessageContent::Text(t) => {
                    t.starts_with(PLACEHOLDER_PREFIX) && t.contains(handle)
                }
                _ => false,
            }),
            _ => false,
        }
    }

    fn find_placeholder_index(thread: &Thread, handle: &str) -> Option<usize> {
        thread.messages().iter().enumerate().find_map(|(i, m)| {
            if Self::message_has_placeholder(m, handle) {
                Some(i)
            } else {
                None
            }
        })
    }

    fn find_placeholder_by_scan(messages: &[crate::Message], handle: &str) -> Option<usize> {
        messages.iter().enumerate().find_map(|(i, m)| {
            if Self::message_has_placeholder(m, handle) {
                Some(i)
            } else {
                None
            }
        })
    }
}

// ============================================================================
// AgentTool Trait Implementation
// ============================================================================

impl AgentTool for MemoryTool {
    type Input = MemoryToolInput;
    type Output = String;

    fn name() -> &'static str {
        "memory"
    }

    fn kind() -> ToolKind {
        ToolKind::Other
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        if let Ok(i) = input {
            match i.operation {
                MemoryOperation::Store => "Archive messages".into(),
                MemoryOperation::Load => "Load memory".into(),
                MemoryOperation::List => "List archives".into(),
                MemoryOperation::Restore => "Restore archive".into(),
                MemoryOperation::Prune => "Prune archives".into(),
            }
        } else {
            "Memory operation".into()
        }
    }

    fn run(
        self: Arc<Self>,
        input: Self::Input,
        _event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output>> {
        Task::ready(self.execute(input, cx))
    }
}
