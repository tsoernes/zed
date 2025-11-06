use std::{path::Path, time::Duration};

use anyhow::Result;
use gpui::{App, AsyncApp};
use smol::Timer;

use assistant_tools::chat_history_adapter;
use serde_json::Value;
use chat_history_tools::{ChatHistoryToolApi, ChatHistoryTools};

use agent::thread_store::{
    legacy_load_thread, legacy_threads_metadata, SerializedMessageSegment, SerializedThreadMetadata,
};

/// Schedules a one–time deferred import of legacy threads into the chat_history persistence.
///
/// Why deferred:
/// The legacy ThreadsDatabase and ThreadStore populate after workspace / project initialization.
/// Importing earlier yields an empty list. This polling approach waits until threads appear or
/// a timeout elapses.
///
/// Behavior:
/// 1. If a sentinel file exists, import is skipped.
/// 2. Polls up to MAX_ATTEMPTS for any legacy threads.
/// 3. If existing persisted chats already have messages, import is skipped (writes sentinel).
/// 4. Replays messages (user + assistant) oldest -> newest to preserve chronology.
/// 5. Includes "thinking" segments prefixed with `[Thought] `, skips redacted.
/// 6. Infers project_id from first worktree snapshot basename (fallback "legacy").
/// 7. Adds tag "imported", preserves title + summary.
/// 8. Writes sentinel with counts to prevent re–import.
///
/// Errors during individual thread/message processing are logged and skipped.
pub fn schedule_legacy_import(cx: &mut App) {
    // Clone minimal state into async task.
    cx.spawn({
        async move |cx: &mut AsyncApp| {
            if let Err(err) = run_import(cx).await {
                ::log::error!("Legacy import failed: {err}");
            }
        }
    })
    .detach();
}

const MAX_ATTEMPTS: usize = 12;
const POLL_INTERVAL_MS: u64 = 500;
const SENTINEL_NAME: &str = ".legacy_import_done";

async fn run_import(cx: &mut AsyncApp) -> Result<()> {
    let sentinel_path = paths::database_dir()
        .join("chat_history")
        .join(SENTINEL_NAME);

    if std::fs::metadata(&sentinel_path).is_ok() {
        ::log::info!("Legacy import: sentinel present, skipping");
        return Ok(());
    }

    // Poll for thread metadata.
    let threads = poll_for_threads(cx).await?;
    if threads.is_empty() {
        ::log::info!("Legacy import: no legacy threads found after polling");
        write_sentinel(&sentinel_path, 0, 0)?;
        return Ok(());
    }

    let adapter = match chat_history_adapter() {
        Some(a) => a,
        None => {
            ::log::warn!("Legacy import: chat_history adapter unavailable");
            return Ok(());
        }
    };

    // Skip if any existing chat already has messages.
    if existing_chats_have_messages(&adapter).await? {
        ::log::info!("Legacy import: existing chats contain messages; skipping import");
        write_sentinel(&sentinel_path, 0, 0)?;
        return Ok(());
    }

    // Import oldest first (reverse chronological order of metadata list).
    let mut imported_threads = 0usize;
    let mut imported_messages = 0usize;

    for meta in threads.iter().rev() {
        let serialized = match load_serialized_thread(cx, meta).await {
            Ok(Some(s)) => s,
            Ok(None) => {
                ::log::warn!("Legacy import: thread {} vanished", meta.id);
                continue;
            }
            Err(err) => {
                ::log::warn!("Legacy import: load error for {}: {err}", meta.id);
                continue;
            }
        };

        let project_id = infer_project_id(&serialized)
            .unwrap_or_else(|| "legacy".to_string());

        let title_opt = if serialized.summary.is_empty() {
            None
        } else {
            Some(serialized.summary.to_string())
        };

        let (chat_created, message_count) =
            replay_messages(&adapter, &project_id, title_opt.as_ref(), &serialized).await?;

        if chat_created {
            // Metadata enrichment (tag + summary).
            if let Some(chat_id) = last_created_chat_id(&adapter, &serialized).await? {
                let meta_update = serde_json::json!({
                    "chat_id": chat_id,
                    "tags_add": ["imported"],
                    "archived": false,
                    "pinned": false,
                    "title": title_opt.clone().unwrap_or_else(|| meta.summary.to_string()),
                    "summary": title_opt.clone()
                })
                .to_string();
                let _ = adapter.chat_update_metadata(&meta_update).await;
                imported_threads += 1;
                imported_messages += message_count;
            }
        }
    }

    write_sentinel(&sentinel_path, imported_threads, imported_messages)?;
    ::log::info!(
        "Legacy import complete: imported {} threads, {} messages",
        imported_threads,
        imported_messages
    );
    Ok(())
}

async fn poll_for_threads(
    cx: &mut AsyncApp,
) -> Result<Vec<SerializedThreadMetadata>> {
    for attempt in 0..MAX_ATTEMPTS {
        let meta_task = cx.update(|cx| legacy_threads_metadata(cx)).ok();
        let list = if let Some(task) = meta_task {
            match task.await {
                Ok(v) => v,
                Err(err) => {
                    ::log::warn!("Legacy import: metadata retrieval error attempt {attempt}: {err}");
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };
        if !list.is_empty() {
            return Ok(list);
        }
        Timer::after(Duration::from_millis(POLL_INTERVAL_MS)).await;
    }
    Ok(Vec::new())
}

async fn existing_chats_have_messages(adapter: &ChatHistoryTools) -> Result<bool> {
    let raw = adapter.chat_list(r#"{"limit":200}"#).await;
    let parsed: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return Ok(false),
    };
    let chats: Vec<Value> = parsed
        .get("chats")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(chats.iter().any(|chat| {
        chat.get("total_messages")
            .and_then(|m| m.as_u64())
            .unwrap_or(0) > 0
    }))
}

async fn load_serialized_thread(
    cx: &mut AsyncApp,
    meta: &SerializedThreadMetadata,
) -> Result<Option<agent::thread_store::SerializedThread>> {
    let load_task = cx
        .update(|cx| legacy_load_thread(cx, meta.id.clone()))
        .ok();
    if let Some(task) = load_task {
        match task.await {
            Ok(opt) => Ok(opt),
            Err(err) => {
                ::log::warn!("Legacy import: error loading thread {}: {err}", meta.id);
                Ok(None)
            }
        }
    } else {
        Ok(None)
    }
}

fn infer_project_id(
    serialized: &agent::thread_store::SerializedThread,
) -> Option<String> {
    let snap = serialized.initial_project_snapshot.as_ref()?;
    let first = snap.worktree_snapshots.first()?;
    let p = Path::new(&first.worktree_path);
    p.file_name().and_then(|n| n.to_str()).map(|s| s.to_string())
}

async fn replay_messages(
    adapter: &ChatHistoryTools,
    project_id: &str,
    title_opt: Option<&String>,
    serialized: &agent::thread_store::SerializedThread,
) -> Result<(bool, usize)> {
    let mut chat_id: Option<String> = None;
    let mut first = true;
    let mut count = 0usize;

    for message in &serialized.messages {
        let role_str = match message.role {
            language_model::Role::User => "User",
            language_model::Role::Assistant => "Assistant",
            _ => continue,
        };

        let mut parts = Vec::new();
        for segment in &message.segments {
            match segment {
                SerializedMessageSegment::Text { text } => parts.push(text.clone()),
                SerializedMessageSegment::Thinking { text, .. } => {
                    parts.push(format!("[Thought] {}", text))
                }
                SerializedMessageSegment::RedactedThinking { .. } => {}
            }
        }
        if parts.is_empty() {
            continue;
        }

        let mut payload = serde_json::json!({
            "chat_id": chat_id,
            "project_id": project_id,
            "role": role_str,
            "content": parts.join("\n"),
        });
        if first {
            if let Some(t) = title_opt {
                payload["title"] = Value::String(t.clone());
            }
            first = false;
        }

        let raw = adapter.chat_append(&payload.to_string()).await;
        if chat_id.is_none() {
            if let Ok(val) = serde_json::from_str::<Value>(&raw) {
                if val.get("ok").and_then(|v| v.as_bool()) == Some(true) {
                    if let Some(cid) = val
                        .get("chat")
                        .and_then(|c| c.get("chat_id"))
                        .and_then(|cid| cid.as_str())
                    {
                        chat_id = Some(cid.to_string());
                    }
                } else {
                    ::log::warn!(
                        "Legacy import: first append failed (summary='{}'): {}",
                        title_opt.unwrap_or(&String::new()),
                        raw
                    );
                    break;
                }
            }
        }
        count += 1;
    }

    Ok((chat_id.is_some(), count))
}

async fn last_created_chat_id(
    adapter: &ChatHistoryTools,
    _serialized: &agent::thread_store::SerializedThread,
) -> Result<Option<String>> {
    let raw = adapter.chat_list(r#"{"limit":10}"#).await;
    let parsed: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let chats: Vec<Value> = parsed
        .get("chats")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(chats
        .first()
        .and_then(|chat| chat.get("chat_id"))
        .and_then(|cid| cid.as_str())
        .map(|s| s.to_string()))
}

fn write_sentinel(path: &Path, threads: usize, messages: usize) -> Result<()> {
    let data = format!("threads={threads},messages={messages}");
    std::fs::write(path, data)?;
    Ok(())
}
