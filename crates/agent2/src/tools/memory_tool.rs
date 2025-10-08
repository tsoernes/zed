use std::{
    collections::{HashMap, HashSet},
    fs,
    sync::Arc,
    time::SystemTime,
};

use anyhow::{Result, anyhow, bail};
use gpui::{App, SharedString, Task, WeakEntity};
use once_cell::sync::OnceCell;
use parking_lot::Mutex;
use regex::Regex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    AgentTool, ToolCallEventStream,
    thread::{AgentMessage, AgentMessageContent, Message, Thread, UserMessage, UserMessageContent},
    token_usage,
};
use acp_thread::UserMessageId;
use agent_client_protocol::ToolKind;
use language_model::{
    LanguageModelRequest, LanguageModelRequestMessage, LanguageModelToolChoice, MessageContent,
    Role,
};
use paths;

/// Operations supported by the memory tool.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryOperation {
    Store,
    Load,
    List,
    Restore,
    Prune,
    Stats,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
    #[serde(default)]
    allow_overlap: bool,
    #[serde(default)]
    json: bool,
}

fn default_preview_chars() -> usize {
    160
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ArchivedMemory {
    handle: String,
    start_index: usize,
    end_index: usize,
    summary: Option<String>,
    char_count: usize,
    token_estimate: usize,
    per_message_tokens: Option<Vec<usize>>,
    messages: Vec<LanguageModelRequestMessage>,
    created_at: SystemTime,
    restored_count: usize,
    last_restored_at: Option<SystemTime>,
}

static STORE: OnceCell<Mutex<HashMap<String, ArchivedMemory>>> = OnceCell::new();
static STORE_LOADED: OnceCell<()> = OnceCell::new();

fn store() -> &'static Mutex<HashMap<String, ArchivedMemory>> {
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn store_file_path() -> std::path::PathBuf {
    paths::data_dir().join("agent2_memory_store.json")
}

fn ensure_store_loaded() {
    if STORE_LOADED.get().is_some() {
        return;
    }
    if let Err(err) = load_persistent_store() {
        log::warn!("memory_tool: failed to load persistent store: {err:?}");
    }
    let _ = STORE_LOADED.set(());
}

fn load_persistent_store() -> Result<()> {
    let path = store_file_path();
    if !path.exists() {
        return Ok(());
    }
    let data = fs::read_to_string(&path)?;
    if data.trim().is_empty() {
        return Ok(());
    }
    #[derive(Deserialize)]
    struct PersistedEntry {
        handle: String,
        archived: ArchivedMemory,
    }
    let entries: Vec<PersistedEntry> = serde_json::from_str(&data)?;
    let mut guard = store().lock();
    for e in entries {
        guard.entry(e.handle).or_insert(e.archived);
    }
    Ok(())
}

fn persist_store() -> Result<()> {
    #[derive(Serialize)]
    struct PersistedEntry<'a> {
        handle: &'a String,
        archived: &'a ArchivedMemory,
    }
    let guard = store().lock();
    let rows: Vec<PersistedEntry> = guard
        .iter()
        .map(|(h, a)| PersistedEntry {
            handle: h,
            archived: a,
        })
        .collect();
    let serialized = serde_json::to_string_pretty(&rows)?;
    fs::write(store_file_path(), serialized)?;
    Ok(())
}

fn placeholder_regex() -> &'static Regex {
    static R: OnceCell<Regex> = OnceCell::new();
    R.get_or_init(|| {
        Regex::new(r#"^\[\[memory archived handle=(?P<handle>\S+) range=(?P<start>\d+)\.\.(?P<end>\d+) messages=(?P<count>\d+) chars=(?P<chars>\d+)(?: tokens=(?P<tokens>\d+))?\]\]"#)
            .expect("placeholder regex")
    })
}

fn heuristic_summary(messages: &[LanguageModelRequestMessage]) -> String {
    // Look for first non-empty textual content.
    for m in messages {
        let text = m
            .content
            .iter()
            .filter_map(|c| c.to_str())
            .collect::<Vec<_>>()
            .join("");
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            let first_line = trimmed.lines().next().unwrap_or(trimmed);
            let mut s = first_line.to_string();
            if s.len() > 140 {
                s.truncate(140);
                s.push_str("...");
            }
            return format!("{s} ({} msgs)", messages.len());
        }
    }
    format!("({} msgs)", messages.len())
}

fn preview(messages: &[LanguageModelRequestMessage], limit: usize) -> String {
    let mut out = String::new();
    let mut used = 0usize;
    for m in messages {
        let chunk = m
            .content
            .iter()
            .filter_map(|c| c.to_str())
            .collect::<Vec<_>>()
            .join("");
        if chunk.is_empty() {
            continue;
        }
        if used + chunk.len() <= limit {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&chunk);
            used += chunk.len();
        } else {
            let remaining = limit.saturating_sub(used);
            if remaining > 16 {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&chunk[..remaining]);
                out.push_str("...(truncated)");
            }
            break;
        }
    }
    out
}

pub struct MemoryTool {
    thread: WeakEntity<Thread>,
}

impl MemoryTool {
    pub fn new(thread: WeakEntity<Thread>) -> Self {
        Self { thread }
    }
}

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
        match input {
            Ok(i) => match i.operation {
                MemoryOperation::Store => "Archive messages".into(),
                MemoryOperation::Load => "Load memory".into(),
                MemoryOperation::List => "List memories".into(),
                MemoryOperation::Restore => "Restore memory".into(),
                MemoryOperation::Prune => "Prune memories".into(),
                MemoryOperation::Stats => "Memory stats".into(),
            },
            Err(_) => "Memory operation".into(),
        }
    }

    fn run(
        self: Arc<Self>,
        mut input: Self::Input,
        _event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output>> {
        input.max_preview_chars = input.max_preview_chars.clamp(40, 400);
        ensure_store_loaded();

        // Snapshot thread state (messages flattened to request messages, and model) up front.
        let (thread_messages, message_count, model_opt) =
            if let Some(thread) = self.thread.upgrade() {
                thread.read_with(cx, |t, _| {
                    let msgs = t
                        .messages()
                        .iter()
                        .map(|m| m.to_request()) // Vec<LanguageModelRequestMessage> per thread Message
                        .collect::<Vec<_>>();
                    (msgs, t.messages().len(), t.model().cloned())
                })
            } else {
                (Vec::new(), 0, None)
            };

        cx.spawn(async move |cx| {
            match input.operation {
                MemoryOperation::Store => {
                    let start = input
                        .start_index
                        .ok_or_else(|| anyhow!(
                            "start_index required for store. Provide both start_index and end_index (inclusive). \
Example: {{\"operation\":\"store\",\"start_index\":0,\"end_index\":0,\"auto\":true}}"
                        ))?;
                    let end = input
                        .end_index
                        .ok_or_else(|| anyhow!(
                            "end_index required for store. Provide both start_index and end_index (inclusive). \
Example: {{\"operation\":\"store\",\"start_index\":0,\"end_index\":3,\"auto\":true}}"
                        ))?;

                    if start > end {
                        bail!(
                            "invalid range: start_index ({start}) > end_index ({end}). \
The range is inclusive. For a single message archive use start_index==end_index."
                        );
                    }
                    if end >= message_count {
                        let highest = message_count.saturating_sub(1);
                        bail!(
                            "end_index {end} out of bounds (message count {message_count}). \
Highest valid index is {highest}. Re-evaluate the intended span after new messages were added."
                        );
                    }

                    if !input.allow_overlap {
                        let guard = store().lock();
                        for a in guard.values() {
                            let overlap = !(end < a.start_index || start > a.end_index);
                            if overlap {
                                bail!(
                                    "overlapping archive with existing range {}..{} (handle={}). \
Pass \"allow_overlap\": true to force, or choose a non-overlapping span.",
                                    a.start_index,
                                    a.end_index,
                                    a.handle
                                );
                            }
                        }
                    }

                    let archived_messages: Vec<LanguageModelRequestMessage> = thread_messages
                        [start..=end]
                        .iter()
                        .flat_map(|v| v.clone())
                        .collect();

                    let char_count: usize = archived_messages
                        .iter()
                        .map(|m| {
                            m.content
                                .iter()
                                .map(|c| match c {
                                    MessageContent::Text(t) => t.len(),
                                    MessageContent::Thinking { text, .. } => text.len(),
                                    MessageContent::RedactedThinking(t) => t.len(),
                                    MessageContent::Image(_) => 7,
                                    MessageContent::ToolUse(_) => 11,
                                    MessageContent::ToolResult(_) => 14,
                                })
                                .sum::<usize>()
                        })
                        .sum();

                    // Construct minimal request for token counting.
                    let base_request = LanguageModelRequest {
                        thread_id: None,
                        prompt_id: None,
                        intent: None,
                        mode: None,
                        messages: archived_messages.clone(),
                        tools: Vec::new(),
                        tool_choice: Some(LanguageModelToolChoice::None),
                        stop: Vec::new(),
                        temperature: None,
                        thinking_allowed: false,
                    };

                    // Token counting: attempt batched precise per-message first, fallback progressively.
                    let (per_message_tokens, total_tokens) = if let Some(model) = &model_opt {
                        let batched = token_usage::precise_per_message_tokens_batched(
                            model,
                            &base_request,
                            &base_request.messages,
                            &*cx,
                            4,
                        )
                        .await
                        .ok();
                        if let Some((pm, total)) = batched {
                            (Some(pm), total)
                        } else if let Ok((pm, total)) = token_usage::precise_per_message_tokens(
                            model,
                            &base_request,
                            &base_request.messages,
                            &*cx,
                        )
                        .await
                        {
                            (Some(pm), total)
                        } else {
                            // Fallback to heuristic
                            let est =
                                token_usage::heuristic_token_count(&base_request.messages) as usize;
                            (None, est)
                        }
                    } else {
                        let est =
                            token_usage::heuristic_token_count(&base_request.messages) as usize;
                        (None, est)
                    };

                    let summary = if let Some(s) = input.summary.take() {
                        Some(s)
                    } else if input.auto {
                        Some(heuristic_summary(&archived_messages))
                    } else {
                        None
                    };

                    let handle = format!("mem://{}", Uuid::new_v4());

                    let preview_text =
                        preview(&archived_messages, input.max_preview_chars.min(16_000));

                    let archived = ArchivedMemory {
                        handle: handle.clone(),
                        start_index: start,
                        end_index: end,
                        summary: summary.clone(),
                        char_count,
                        token_estimate: total_tokens,
                        per_message_tokens,
                        messages: archived_messages.clone(),
                        created_at: SystemTime::now(),
                        restored_count: 0,
                        last_restored_at: None,
                    };
                    {
                        let mut guard = store().lock();
                        guard.insert(handle.clone(), archived);
                    }
                    persist_store().ok();

                    let mut placeholder = format!(
                        "[[memory archived handle={} range={}..{} messages={} chars={} tokens={}]]",
                        handle,
                        start,
                        end,
                        (end - start + 1),
                        char_count,
                        total_tokens
                    );
                    if let Some(s) = &summary {
                        placeholder.push_str(&format!("\nSummary: {s}"));
                    }
                    if !preview_text.is_empty() {
                        placeholder.push_str(&format!("\nPreview: {}", preview_text));
                    }

                    // Mutate thread replacing range with placeholder.
                    if let Some(thread) = self.thread.upgrade() {
                        let _ = thread.update(cx, |t, thread_cx| {
                            t.set_placeholder(start, placeholder.clone(), thread_cx)?;
                            for idx in (start + 1..=end).rev() {
                                t.remove_message(idx, thread_cx)?;
                            }
                            Ok::<(), anyhow::Error>(())
                        });
                    }

                    if input.json {
                        let payload = serde_json::json!({
                            "operation":"store",
                            "handle": handle,
                            "range": format!("{}..{}", start, end),
                            "messages": end - start + 1,
                            "chars": char_count,
                            "tokens": total_tokens,
                            "summary": summary,
                            "placeholder": placeholder,
                            "preview": preview_text,
                            "per_message_tokens": archived_messages.len() > 0
                                && archived_messages.len() <= 256
                                && store().lock().get(&handle).and_then(|a| a.per_message_tokens.clone()).is_some()
                        });
                        return Ok(payload.to_string());
                    }

                    Ok(format!(
                        "Archived messages {}..{} -> {}\nPlaceholder inserted at index {}\n{}",
                        start, end, handle, start, placeholder
                    ))
                }
                MemoryOperation::Load => {
                    let handle = input
                        .memory_handle
                        .ok_or_else(|| anyhow!("memory_handle required for load"))?;
                    let guard = store().lock();
                    let a = guard
                        .get(&handle)
                        .ok_or_else(|| anyhow!("unknown memory handle: {handle}"))?;
                    if input.json {
                        let payload = serde_json::json!({
                            "operation":"load",
                            "handle": handle,
                            "range": format!("{}..{}", a.start_index, a.end_index),
                            "messages": a.messages.len(),
                            "chars": a.char_count,
                            "tokens": a.token_estimate,
                            "summary": a.summary,
                            "restored_count": a.restored_count,
                        });
                        return Ok(payload.to_string());
                    }
                    let mut out = format!(
                        "# Archived Memory: {handle}\nRange: {}..{}\nMessages: {}\nChars: {}\nTokens: {}\n",
                        a.start_index,
                        a.end_index,
                        a.messages.len(),
                        a.char_count,
                        a.token_estimate
                    );
                    if let Some(s) = &a.summary {
                        out.push_str(&format!("Summary: {s}\n"));
                    }
                    out.push_str("\n## Messages\n\n");
                    for (i, m) in a.messages.iter().enumerate() {
                        let body = m
                            .content
                            .iter()
                            .filter_map(|c| c.to_str())
                            .collect::<Vec<_>>()
                            .join("");
                        out.push_str(&format!(
                            "### Message {} (orig idx {}) role={:?}\n{}\n\n",
                            i,
                            a.start_index + i,
                            m.role,
                            body
                        ));
                    }
                    Ok(out)
                }
                MemoryOperation::List => {
                    // Detect placeholders in current thread snapshot (first line of first segment).
                    let mut referenced: HashMap<String, Vec<usize>> = HashMap::new();
                    for (idx, group) in thread_messages.iter().enumerate() {
                        if let Some(first_msg) = group.first() {
                            if let Some(line) = first_msg
                                .content
                                .iter()
                                .filter_map(|c| c.to_str())
                                .find(|s| !s.trim().is_empty())
                                .and_then(|s| s.lines().next())
                            {
                                if let Some(caps) = placeholder_regex().captures(line.trim()) {
                                    if let Some(h) = caps.name("handle") {
                                        referenced
                                            .entry(h.as_str().to_string())
                                            .or_default()
                                            .push(idx);
                                    }
                                }
                            }
                        }
                    }

                    let guard = store().lock();
                    if guard.is_empty() {
                        if input.json {
                            return Ok(serde_json::json!({
                                "archives": [],
                                "orphaned": []
                            })
                            .to_string());
                        }
                        return Ok("# Archived Memories\n\nNo archived memories.\n".into());
                    }

                    struct Row<'a> {
                        handle: &'a str,
                        range: String,
                        messages: usize,
                        referenced: bool,
                        summary: String,
                        tokens: usize,
                    }

                    let mut rows = Vec::new();
                    for a in guard.values() {
                        rows.push(Row {
                            handle: a.handle.as_str(),
                            range: format!("{}..{}", a.start_index, a.end_index),
                            messages: a.messages.len(),
                            referenced: referenced.contains_key(&a.handle),
                            summary: a
                                .summary
                                .as_ref()
                                .map(|s| s.replace('\n', " "))
                                .unwrap_or_default(),
                            tokens: a.token_estimate,
                        });
                    }
                    rows.sort_by_key(|r| r.handle.to_string());

                    let mut orphaned = Vec::new();
                    for (h, ix) in referenced {
                        if !guard.contains_key(&h) {
                            for i in ix {
                                orphaned.push((h.clone(), i));
                            }
                        }
                    }

                    if input.json {
                        let payload = serde_json::json!({
                            "archives": rows.iter().map(|r| serde_json::json!({
                                "handle": r.handle,
                                "range": r.range,
                                "messages": r.messages,
                                "referenced": r.referenced,
                                "summary": r.summary,
                                "tokens": r.tokens
                            })).collect::<Vec<_>>(),
                            "orphaned": orphaned.iter().map(|(h,i)| serde_json::json!({
                                "handle": h,
                                "index": i
                            })).collect::<Vec<_>>()
                        });
                        return Ok(payload.to_string());
                    }

                    let mut out = String::from(
                        "# Archived Memories\n\nHandle | Range | Count | Tokens | Ref | Summary\n---|---|---|---|---|---\n",
                    );
                    for r in rows {
                        out.push_str(&format!(
                            "{} | {} | {} | {} | {} | {}\n",
                            r.handle,
                            r.range,
                            r.messages,
                            r.tokens,
                            if r.referenced { "yes" } else { "no" },
                            r.summary
                        ));
                    }
                    if !orphaned.is_empty() {
                        out.push_str("\nOrphaned placeholders:\n");
                        for (h, i) in orphaned {
                            out.push_str(&format!("- index {} handle {}\n", i, h));
                        }
                    }
                    Ok(out)
                }
                MemoryOperation::Restore => {
                    let handle = input
                        .memory_handle
                        .ok_or_else(|| anyhow!("memory_handle required for restore"))?;
                    let (archived, placeholder_index_opt) = {
                        let guard = store().lock();
                        let a = guard
                            .get(&handle)
                            .ok_or_else(|| anyhow!("unknown memory handle: {handle}"))?
                            .clone();

                        // Search for placeholder by handle.
                        let mut pi = None;
                        'outer: for (idx, group) in thread_messages.iter().enumerate() {
                            for m in group {
                                if let Some(line) = m
                                    .content
                                    .iter()
                                    .filter_map(|c| c.to_str())
                                    .find(|s| !s.trim().is_empty())
                                    .and_then(|s| s.lines().next())
                                {
                                    if let Some(caps) = placeholder_regex().captures(line.trim()) {
                                        if caps.name("handle").map(|h| h.as_str())
                                            == Some(handle.as_str())
                                        {
                                            pi = Some(idx);
                                            break 'outer;
                                        }
                                    }
                                }
                            }
                        }
                        (a, pi)
                    };

                    // Determine target insertion.
                    let target_index = input
                        .restore_insert_index
                        .or(placeholder_index_opt)
                        .unwrap_or(message_count);

                    // Reconstruct messages (full segment fidelity where possible).
                    // We persisted full LanguageModelRequestMessage objects; we now map them
                    // back into high-level thread Message variants, recreating structured
                    // segments (thinking, tool use, redacted thinking, images) where feasible.
                    let mut new_messages: Vec<Message> = Vec::new();
                    for lm in &archived.messages {
                        // Skip entirely empty messages
                        if lm
                            .content
                            .iter()
                            .all(|c| c.to_str().map(|s| s.trim().is_empty()).unwrap_or(false))
                        {
                            continue;
                        }

                        match lm.role {
                            Role::Assistant => {
                                // Rebuild AgentMessage with structured segments.
                                let mut agent_segments = Vec::new();
                                for seg in &lm.content {
                                    match seg {
                                        MessageContent::Text(t) => {
                                            agent_segments
                                                .push(AgentMessageContent::Text(t.clone()));
                                        }
                                        MessageContent::Thinking { text, signature } => {
                                            agent_segments.push(AgentMessageContent::Thinking {
                                                text: text.clone(),
                                                signature: signature.clone(),
                                            });
                                        }
                                        MessageContent::RedactedThinking(t) => {
                                            agent_segments.push(
                                                AgentMessageContent::RedactedThinking(t.clone()),
                                            );
                                        }
                                        MessageContent::ToolUse(tool_use) => {
                                            // Preserve the tool use marker; tool_results map
                                            // cannot be reconstructed from request alone, so we omit.
                                            agent_segments.push(AgentMessageContent::ToolUse(
                                                tool_use.clone(),
                                            ));
                                        }
                                        // ToolResult segments are represented as a *user* role
                                        // message in the original transformation pipeline; ignore here.
                                        MessageContent::ToolResult(_) => {}
                                        MessageContent::Image(img) => {
                                            // Images are not representable directly as AgentMessageContent
                                            // in this thread model; include a textual marker.
                                            let marker = format!("<image size={:?} />", img.size);
                                            agent_segments.push(AgentMessageContent::Text(marker));
                                        }
                                    }
                                }
                                if !agent_segments.is_empty() {
                                    new_messages.push(Message::Agent(AgentMessage {
                                        content: agent_segments,
                                        tool_results: Default::default(),
                                    }));
                                }
                            }
                            Role::User | Role::System => {
                                // Rebuild as UserMessage. We map any ToolResult or Image segments
                                // into textual representations so that information is retained.
                                let mut user_segments: Vec<UserMessageContent> = Vec::new();
                                for seg in &lm.content {
                                    match seg {
                                        MessageContent::Text(t) => {
                                            if !t.trim().is_empty() {
                                                user_segments
                                                    .push(UserMessageContent::Text(t.clone()));
                                            }
                                        }
                                        MessageContent::Thinking { text, .. } => {
                                            if !text.trim().is_empty() {
                                                user_segments.push(UserMessageContent::Text(
                                                    format!("<think>{}</think>", text),
                                                ));
                                            }
                                        }
                                        MessageContent::RedactedThinking(_) => {
                                            user_segments.push(UserMessageContent::Text(
                                                "<redacted_thinking />".into(),
                                            ));
                                        }
                                        MessageContent::ToolUse(tool_use) => {
                                            user_segments.push(UserMessageContent::Text(format!(
                                                "[[tool_use id={} name={}]]",
                                                tool_use.id, tool_use.name
                                            )));
                                        }
                                        MessageContent::ToolResult(tr) => {
                                            let body = tr
                                                .content
                                                .to_str()
                                                .unwrap_or("<tool_result (non-text)>");
                                            user_segments.push(UserMessageContent::Text(format!(
                                                "[[tool_result tool={} id={}] {}\n]",
                                                tr.tool_name, tr.tool_use_id, body
                                            )));
                                        }
                                        MessageContent::Image(img) => {
                                            // Reconstruct original user image segment instead of degrading to placeholder text.
                                            user_segments
                                                .push(UserMessageContent::Image(img.clone()));
                                        }
                                    }
                                }
                                if !user_segments.is_empty() {
                                    new_messages.push(Message::User(UserMessage {
                                        id: UserMessageId::new(),
                                        content: user_segments,
                                    }));
                                }
                            }
                        }
                    }

                    // Perform thread mutation.
                    if let Some(thread) = self.thread.upgrade() {
                        let _ = thread.update(cx, |t, thread_cx| {
                            let new_messages_len = new_messages.len();
                            t.insert_messages(target_index, new_messages, thread_cx)?;
                            if let Some(pi) = placeholder_index_opt {
                                if input.remove_placeholder {
                                    // After insertion, placeholder may shift if insertion before it.
                                    let adjusted = if target_index <= pi {
                                        pi + new_messages_len
                                    } else {
                                        pi
                                    };
                                    // Bounds check
                                    if adjusted < t.messages().len() {
                                        t.remove_message(adjusted, thread_cx)?;
                                    }
                                } else if let Some(rep) = input.replace_placeholder_with.clone() {
                                    let adjusted = if target_index <= pi {
                                        pi + new_messages_len
                                    } else {
                                        pi
                                    };
                                    if adjusted < t.messages().len() {
                                        t.set_placeholder(adjusted, rep, thread_cx)?;
                                    }
                                }
                            }
                            Ok::<(), anyhow::Error>(())
                        });
                    }

                    {
                        let mut guard = store().lock();
                        if let Some(m) = guard.get_mut(&handle) {
                            m.restored_count += 1;
                            m.last_restored_at = Some(SystemTime::now());
                        }
                    }
                    persist_store().ok();

                    if input.json {
                        let payload = serde_json::json!({
                            "operation":"restore",
                            "handle": handle,
                            "inserted_at": target_index,
                            "restored_messages": archived.messages.len(),
                            "placeholder_removed": input.remove_placeholder,
                            "placeholder_replaced": input.replace_placeholder_with.is_some(),
                        });
                        return Ok(payload.to_string());
                    }

                    let mut out = format!(
                        "Restored {} archived messages from {} at index {}",
                        archived.messages.len(),
                        handle,
                        target_index
                    );
                    if input.remove_placeholder {
                        out.push_str("\nPlaceholder removed.");
                    } else if input.replace_placeholder_with.is_some() {
                        out.push_str("\nPlaceholder replaced.");
                    } else {
                        out.push_str("\nPlaceholder retained.");
                    }
                    Ok(out)
                }
                MemoryOperation::Prune => {
                    // Detect referenced handles.
                    let mut referenced = HashSet::new();
                    for group in &thread_messages {
                        for m in group {
                            if let Some(line) = m
                                .content
                                .iter()
                                .filter_map(|c| c.to_str())
                                .find(|s| !s.trim().is_empty())
                                .and_then(|s| s.lines().next())
                            {
                                if let Some(caps) = placeholder_regex().captures(line.trim()) {
                                    if let Some(h) = caps.name("handle") {
                                        referenced.insert(h.as_str().to_string());
                                    }
                                }
                            }
                        }
                    }
                    let mut guard = store().lock();
                    let before = guard.len();
                    let keys: Vec<String> = guard.keys().cloned().collect();
                    let mut pruned = 0usize;
                    for h in keys {
                        if !referenced.contains(&h) {
                            guard.remove(&h);
                            pruned += 1;
                        }
                    }
                    if pruned > 0 {
                        persist_store().ok();
                    }
                    if input.json {
                        return Ok(serde_json::json!({
                            "operation":"prune",
                            "initial":before,
                            "pruned":pruned,
                            "remaining":guard.len(),
                            "referenced_placeholders":referenced.len()
                        })
                        .to_string());
                    }
                    Ok(format!(
                        "Prune complete. initial={} pruned={} remaining={} referenced_placeholders={}",
                        before,
                        pruned,
                        guard.len(),
                        referenced.len()
                    ))
                }
                MemoryOperation::Stats => {
                    // Aggregate archive stats
                    let guard = store().lock();
                    let mut total_archives = 0usize;
                    let mut total_messages = 0usize;
                    let mut total_chars = 0usize;
                    let mut total_tokens = 0usize;
                    let mut restored_archives = 0usize;
                    for a in guard.values() {
                        total_archives += 1;
                        total_messages += a.messages.len();
                        total_chars += a.char_count;
                        total_tokens += a.token_estimate;
                        if a.restored_count > 0 {
                            restored_archives += 1;
                        }
                    }

                    // Active context token usage (heuristic for current thread).
                    let active_request_messages: Vec<LanguageModelRequestMessage> =
                        thread_messages.iter().flat_map(|v| v.clone()).collect();
                    let heuristic_active_tokens =
                        token_usage::heuristic_token_count(&active_request_messages) as usize;
                    let precise_active_tokens = if let Some(model) = &model_opt {
                        let base = LanguageModelRequest {
                            thread_id: None,
                            prompt_id: None,
                            intent: None,
                            mode: None,
                            messages: active_request_messages.clone(),
                            tools: Vec::new(),
                            tool_choice: Some(LanguageModelToolChoice::None),
                            stop: Vec::new(),
                            temperature: None,
                            thinking_allowed: false,
                        };
                        token_usage::precise_tokens_for_slice(model, &base, &base.messages, &*cx)
                            .await
                    } else {
                        heuristic_active_tokens
                    };

                    let avg_chars = if total_archives > 0 {
                        total_chars as f64 / total_archives as f64
                    } else {
                        0.0
                    };
                    let avg_tokens = if total_archives > 0 {
                        total_tokens as f64 / total_archives as f64
                    } else {
                        0.0
                    };

                    if input.json {
                        let payload = serde_json::json!({
                            "operation":"stats",
                            "archives": total_archives,
                            "archived_messages": total_messages,
                            "archived_chars": total_chars,
                            "archived_tokens": total_tokens,
                            "avg_chars_per_archive": avg_chars,
                            "avg_tokens_per_archive": avg_tokens,
                            "restored_archives": restored_archives,
                            "active_tokens_precise": precise_active_tokens,
                            "active_tokens_heuristic": heuristic_active_tokens,
                        });
                        return Ok(payload.to_string());
                    }

                    let mut out = String::new();
                    out.push_str("# Memory & Context Stats\n\n");
                    out.push_str("## Active Context\n");
                    out.push_str(&format!(
                        "Active tokens (precise/heuristic): {}/{}\n",
                        precise_active_tokens, heuristic_active_tokens
                    ));

                    // Attempt per‑message precise distribution for recommendation (best effort).
                    let mut recommendation = String::new();
                    if let Some(model) = &model_opt {
                        let base = LanguageModelRequest {
                            thread_id: None,
                            prompt_id: None,
                            intent: None,
                            mode: None,
                            messages: active_request_messages.clone(),
                            tools: Vec::new(),
                            tool_choice: Some(LanguageModelToolChoice::None),
                            stop: Vec::new(),
                            temperature: None,
                            thinking_allowed: false,
                        };
                        if let Ok((per, total)) = token_usage::precise_per_message_tokens_batched(
                            model,
                            &base,
                            &base.messages,
                            &*cx,
                            6,
                        )
                        .await
                        {
                            let max_tokens = model.max_token_count() as usize;
                            if max_tokens > 0 && total > 0 {
                                let usage_pct =
                                    (total as f64 / max_tokens as f64 * 100.0).clamp(0.0, 100.0);
                                if usage_pct > 70.0 {
                                    // Target 60% of capacity.
                                    let target = (max_tokens as f64 * 0.60) as usize;
                                    let mut cum = 0usize;
                                    let mut end_ix = None;
                                    for (i, tks) in per.iter().enumerate() {
                                        cum += *tks;
                                        if total.saturating_sub(cum) <= target {
                                            end_ix = Some(i);
                                            break;
                                        }
                                    }
                                    if let Some(eix) = end_ix {
                                        // Derive a seed summary line from first non-empty text.
                                        let mut seed = String::new();
                                        for msg in &base.messages[0..=eix] {
                                            let text = msg
                                                .content
                                                .iter()
                                                .filter_map(|c| c.to_str())
                                                .find(|s| !s.trim().is_empty());
                                            if let Some(t) = text {
                                                seed = t.lines().next().unwrap_or(t).to_string();
                                                break;
                                            }
                                        }
                                        if seed.len() > 140 {
                                            seed.truncate(140);
                                            seed.push_str("...");
                                        }
                                        let freed = cum;
                                        let projected = total.saturating_sub(freed);
                                        let projected_pct = if max_tokens > 0 {
                                            (projected as f64 / max_tokens as f64 * 100.0)
                                                .clamp(0.0, 100.0)
                                        } else {
                                            0.0
                                        };
                                        recommendation = format!(
                                            "Recommendation: archive messages 0..{end} (~{freed} tokens) lowering usage {:.2}% -> {:.2}%. Suggested summary seed: \"{} ({count} msgs)\".\nExample store call JSON: {{\"operation\":\"store\",\"start_index\":0,\"end_index\":{end},\"auto\":true}}",
                                            usage_pct,
                                            projected_pct,
                                            seed,
                                            end = eix,
                                            freed = freed,
                                            count = eix + 1
                                        );
                                    }
                                }
                            }
                        }
                    }

                    out.push_str("\n## Archived\n");
                    out.push_str(&format!("Archives: {}\n", total_archives));
                    out.push_str(&format!("Archived messages: {}\n", total_messages));
                    out.push_str(&format!("Archived chars: {}\n", total_chars));
                    out.push_str(&format!("Archived tokens: {}\n", total_tokens));
                    out.push_str(&format!("Avg chars/archive: {:.2}\n", avg_chars));
                    out.push_str(&format!("Avg tokens/archive: {:.2}\n", avg_tokens));
                    out.push_str(&format!(
                        "Restored archives (ever): {}\n",
                        restored_archives
                    ));

                    out.push_str("\n## Action\n");
                    if recommendation.is_empty() {
                        out.push_str("No immediate archival recommendation (usage ≤ 70% or insufficient data).\n");
                    } else {
                        out.push_str(&recommendation);
                        out.push('\n');
                    }

                    Ok(out)
                }
            }
        })
    }
}
