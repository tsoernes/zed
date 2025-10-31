//! Persistence domain for `ChatStore` data.
//! Kept separate from the in‑memory core so consumers without persistence
//! (e.g. tests, minimal builds) can exclude it via a feature gate.

#![cfg(feature = "chat-persistence")]

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use chrono::{DateTime, TimeZone, Utc};
use db::{
    query,
    sqlez::{domain::Domain, thread_safe_connection::ThreadSafeConnection},
    sqlez_macros::sql,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    ChatId, ChatMessage, ChatMetadata, MessageId, MessageRole,
    ChatStore, ChatHistoryConfig, ChatHistoryError,
};

/// Domain wrapper for chat history tables.
pub struct ChatHistoryDomain(ThreadSafeConnection);

impl Domain for ChatHistoryDomain {
    const NAME: &str = "ChatHistoryDomain";

    // Migrations: chats then messages. STRICT for schema discipline.
    const MIGRATIONS: &[&str] = &[
        sql!(
            CREATE TABLE IF NOT EXISTS chats(
                chat_id TEXT PRIMARY KEY,
                project_id TEXT,
                title TEXT,
                summary TEXT,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                total_messages INTEGER NOT NULL,
                total_characters INTEGER NOT NULL,
                token_estimate INTEGER NOT NULL,
                archived INTEGER NOT NULL,
                pinned INTEGER NOT NULL,
                tags TEXT NOT NULL,
                embedding_model TEXT,
                chat_vector TEXT,
                last_summary_at_chars INTEGER NOT NULL
            ) STRICT;
        ),
        sql!(
            CREATE TABLE IF NOT EXISTS chat_messages(
                message_id TEXT PRIMARY KEY,
                chat_id TEXT NOT NULL REFERENCES chats(chat_id) ON DELETE CASCADE,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                token_estimate INTEGER NOT NULL,
                digest TEXT NOT NULL,
                vector TEXT
            ) STRICT;
        ),
        sql!(CREATE INDEX IF NOT EXISTS idx_chat_messages_chat_id ON chat_messages(chat_id);),
        sql!(CREATE INDEX IF NOT EXISTS idx_chat_messages_created_at ON chat_messages(created_at);),
    ];
}

// Static connection handle (requires a scope established by main application initialization).
db::static_connection!(CHAT_HISTORY_DB, ChatHistoryDomain, []);

/// Serializable projections for persistence (stable, decoupled from internal struct evolution).
#[derive(Debug, Serialize, Deserialize)]
struct ChatRow {
    chat_id: String,
    project_id: Option<String>,
    title: Option<String>,
    summary: Option<String>,
    created_at: i64,
    updated_at: i64,
    total_messages: i64,
    total_characters: i64,
    token_estimate: i64,
    archived: i64,
    pinned: i64,
    tags: String,
    embedding_model: Option<String>,
    chat_vector: Option<String>,
    last_summary_at_chars: i64,
}

#[derive(Debug, Serialize, Deserialize)]
struct MessageRow {
    message_id: String,
    chat_id: String,
    role: String,
    content: String,
    created_at: i64,
    token_estimate: i64,
    digest: String,
    vector: Option<String>,
}

impl From<&ChatMetadata> for ChatRow {
    fn from(m: &ChatMetadata) -> Self {
        Self {
            chat_id: m.chat_id.as_str().to_string(),
            project_id: m.project_id.clone(),
            title: m.title.clone(),
            summary: m.summary.clone(),
            created_at: to_unix(m.created_at),
            updated_at: to_unix(m.updated_at),
            total_messages: m.total_messages as i64,
            total_characters: m.total_characters as i64,
            token_estimate: m.token_estimate as i64,
            archived: m.archived as i64,
            pinned: m.pinned as i64,
            tags: serde_json::to_string(&m.tags).unwrap_or_else(|_| "[]".into()),
            embedding_model: m.embedding_model.clone(),
            chat_vector: m
                .chat_vector
                .as_ref()
                .and_then(|v| serde_json::to_string(v).ok()),
            last_summary_at_chars: m.last_summary_at_chars as i64,
        }
    }
}

impl TryFrom<ChatRow> for ChatMetadata {
    type Error = anyhow::Error;
    fn try_from(r: ChatRow) -> Result<Self> {
        let tags: Vec<String> = serde_json::from_str(&r.tags).unwrap_or_default();
        let chat_vector = match r.chat_vector {
            Some(s) => serde_json::from_str(&s).ok(),
            None => None,
        };
        Ok(Self {
            chat_id: ChatId::new(r.chat_id),
            project_id: r.project_id,
            title: r.title,
            summary: r.summary,
            created_at: from_unix(r.created_at),
            updated_at: from_unix(r.updated_at),
            total_messages: r.total_messages as usize,
            total_characters: r.total_characters as usize,
            token_estimate: r.token_estimate as usize,
            archived: r.archived != 0,
            pinned: r.pinned != 0,
            tags,
            embedding_model: r.embedding_model,
            chat_vector,
            last_summary_at_chars: r.last_summary_at_chars as usize,
        })
    }
}

impl From<&ChatMessage> for MessageRow {
    fn from(m: &ChatMessage) -> Self {
        Self {
            message_id: m.message_id.0.clone(),
            chat_id: m.chat_id.as_str().to_string(),
            role: format!("{:?}", m.role).to_lowercase(),
            content: m.content.clone(),
            created_at: to_unix(m.created_at),
            token_estimate: m.token_estimate as i64,
            digest: m.digest.clone(),
            vector: m
                .vector
                .as_ref()
                .and_then(|v| serde_json::to_string(v).ok()),
        }
    }
}

impl TryFrom<MessageRow> for ChatMessage {
    type Error = anyhow::Error;
    fn try_from(r: MessageRow) -> Result<Self> {
        let role = match r.role.as_str() {
            "user" => MessageRole::User,
            "assistant" => MessageRole::Assistant,
            other => return Err(anyhow!("unknown role {other}")),
        };
        let vector = match r.vector {
            Some(s) => serde_json::from_str(&s).ok(),
            None => None,
        };
        Ok(Self {
            message_id: MessageId::new(r.message_id),
            chat_id: ChatId::new(r.chat_id),
            role,
            content: r.content,
            created_at: from_unix(r.created_at),
            token_estimate: r.token_estimate as usize,
            digest: r.digest,
            vector,
        })
    }
}

fn to_unix(dt: DateTime<Utc>) -> i64 {
    dt.timestamp()
}

fn from_unix(ts: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(ts, 0).single().unwrap_or_else(|| {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(ts);
        Utc.timestamp_opt(now, 0).unwrap()
    })
}

impl ChatHistoryDomain {
    query! {
        pub fn upsert_chat(row: ChatRow) -> Result<()> {
            INSERT INTO chats(
                chat_id, project_id, title, summary, created_at, updated_at,
                total_messages, total_characters, token_estimate, archived, pinned,
                tags, embedding_model, chat_vector, last_summary_at_chars
            ) VALUES(
                (:chat_id),(:project_id),(:title),(:summary),(:created_at),(:updated_at),
                (:total_messages),(:total_characters),(:token_estimate),(:archived),(:pinned),
                (:tags),(:embedding_model),(:chat_vector),(:last_summary_at_chars)
            )
            ON CONFLICT(chat_id) DO UPDATE SET
                project_id=excluded.project_id,
                title=excluded.title,
                summary=excluded.summary,
                updated_at=excluded.updated_at,
                total_messages=excluded.total_messages,
                total_characters=excluded.total_characters,
                token_estimate=excluded.token_estimate,
                archived=excluded.archived,
                pinned=excluded.pinned,
                tags=excluded.tags,
                embedding_model=excluded.embedding_model,
                chat_vector=excluded.chat_vector,
                last_summary_at_chars=excluded.last_summary_at_chars
        }
    }

    query! {
        pub fn insert_message(row: MessageRow) -> Result<()> {
            INSERT OR IGNORE INTO chat_messages(
                message_id, chat_id, role, content, created_at,
                token_estimate, digest, vector
            ) VALUES(
                (:message_id),(:chat_id),(:role),(:content),(:created_at),
                (:token_estimate),(:digest),(:vector)
            )
        }
    }

    query! {
        pub fn select_chats() -> Result<Vec<ChatRow>> {
            SELECT * FROM chats ORDER BY updated_at DESC
        }
    }

    query! {
        pub fn select_chat(chat_id: String) -> Result<Option<ChatRow>> {
            SELECT * FROM chats WHERE chat_id = (:chat_id)
        }
    }

    query! {
        pub fn select_messages(chat_id: String) -> Result<Vec<MessageRow>> {
            SELECT * FROM chat_messages WHERE chat_id = (:chat_id) ORDER BY created_at ASC
        }
    }

    query! {
        pub fn delete_chat(chat_id: String) -> Result<()> {
            DELETE FROM chats WHERE chat_id = (:chat_id)
        }
    }
}

/// Persist entire in-memory store (full snapshot).
pub fn persist_store(store: &crate::SharedChatStore) -> Result<()> {
    let handle = &*CHAT_HISTORY_DB;
    store.persist_all(|meta, msg| {
        handle.upsert_chat(ChatRow::from(meta))?;
        if !msg.content.is_empty() || msg.vector.is_some() {
            handle.insert_message(MessageRow::from(msg))?;
        }
        Ok(())
    })?;
    Ok(())
}

/// Load all chats/messages into an empty store (replaces existing data).
pub fn load_into(store: &crate::SharedChatStore, config: &ChatHistoryConfig) -> Result<()> {
    let handle = &*CHAT_HISTORY_DB;
    let rows = handle.select_chats()?;
    let mut assembled = Vec::new();
    for row in rows {
        let meta = ChatMetadata::try_from(row)?;
        let msgs_rows = handle.select_messages(meta.chat_id.as_str().to_string())?;
        let mut msgs = Vec::with_capacity(msgs_rows.len());
        for mr in msgs_rows {
            msgs.push(ChatMessage::try_from(mr)?);
        }
        assembled.push((meta, msgs));
    }
    store.load_from(assembled);
    // After load, re-run backend auto-init in case config changed.
    {
        let mut guard = store.lock();
        guard.set_config(config.clone());
        guard.auto_init_backend();
    }
    Ok(())
}

/// Incremental append persistence (call after each successful append).
pub fn persist_append(meta: &ChatMetadata, msg: &ChatMessage) -> Result<()> {
    let handle = &*CHAT_HISTORY_DB;
    handle.upsert_chat(ChatRow::from(meta))?;
    handle.insert_message(MessageRow::from(msg))?;
    Ok(())
}

/// Partial metadata update persistence (call after update_metadata).
pub fn persist_metadata(meta: &ChatMetadata) -> Result<()> {
    let handle = &*CHAT_HISTORY_DB;
    handle.upsert_chat(ChatRow::from(meta))?;
    Ok(())
}

/// Simple health check—ensures schema accessible.
pub fn health_check() -> Result<()> {
    let _ = CHAT_HISTORY_DB.select_chats()?;
    Ok(())
}
