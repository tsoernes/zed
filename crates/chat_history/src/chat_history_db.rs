#![cfg(feature = "chat-persistence")]
//! Chat history persistence using a single JSON snapshot stored in the internal key‑value store.
//!
//! This implementation favors simplicity (Option 1):
//! - All chats and messages are serialized into a single JSON blob under a fixed key.
//! - On append / metadata update we reserialize the entire store (O(n)).
//! - Errors are logged but never propagated to callers so runtime behavior (retrieval etc.)
//!   is not blocked by persistence failures.
//!
//! Future improvements (if needed):
//! - Introduce incremental per‑chat keys (`chat_history_chat_<id>`) to reduce snapshot size.
//! - Maintain a write queue / debounce mechanism to avoid frequent rewrites on high‑volume append.
//! - Add checksum / compression (gzip) for large histories.
//!
//! The goal here is correctness + clarity over performance. For large installations consider
//! migrating back to normalized tables or a chunked key strategy.

use anyhow::{Result, anyhow};
use log::{debug, error};
use serde::{Deserialize, Serialize};

use crate::{ChatMessage, ChatMetadata, SharedChatStore};

/// Storage key for the full snapshot.
const SNAPSHOT_KEY: &str = "chat_history_snapshot_v1";

/// Top‑level serialized structure.
#[derive(Debug, Serialize, Deserialize)]
struct Snapshot {
    version: u32,
    chats: Vec<ChatRecord>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ChatRecord {
    metadata: ChatMetadata,
    messages: Vec<ChatMessage>,
}

/// Persist the entire in‑memory store as a snapshot.
/// This function logs errors and returns Ok(()) regardless to avoid blocking core logic.
pub fn persist_snapshot(store: &SharedChatStore) {
    let guard = store.lock();
    let mut records = Vec::with_capacity(guard.chats.len());
    for meta in guard.chats.values() {
        let msgs = guard
            .messages
            .get(&meta.chat_id)
            .map(|v| v.clone())
            .unwrap_or_default();
        records.push(ChatRecord {
            metadata: meta.clone(),
            messages: msgs,
        });
    }
    drop(guard);

    let snapshot = Snapshot {
        version: 1,
        chats: records,
    };

    match serde_json::to_string(&snapshot) {
        Ok(json) => {
            if let Err(e) = write_kvp(SNAPSHOT_KEY, json) {
                error!("chat_history: failed writing snapshot: {e:?}");
            } else {
                debug!(
                    "chat_history: snapshot persisted ({} chats)",
                    snapshot.chats.len()
                );
            }
        }
        Err(e) => error!("chat_history: failed to serialize snapshot: {e:?}"),
    }
}

/// Load snapshot into an *empty* store. Existing in‑memory data is cleared.
/// Returns Ok(()) even if load fails (with logged errors) to avoid blocking startup.
pub fn load_snapshot(store: &SharedChatStore) -> Result<()> {
    let json = match read_kvp(SNAPSHOT_KEY) {
        Ok(Some(j)) => j,
        Ok(None) => {
            debug!("chat_history: no snapshot found");
            return Ok(());
        }
        Err(e) => {
            error!("chat_history: read snapshot error: {e:?}");
            return Ok(());
        }
    };

    let snapshot: Snapshot = match serde_json::from_str(&json) {
        Ok(s) => s,
        Err(e) => {
            error!("chat_history: invalid snapshot JSON: {e:?}");
            return Ok(());
        }
    };

    if snapshot.version != 1 {
        error!(
            "chat_history: unsupported snapshot version {} (expected 1)",
            snapshot.version
        );
        return Ok(());
    }

    let mut guard = store.lock();
    guard.chats.clear();
    guard.messages.clear();

    for record in snapshot.chats {
        let cid = record.metadata.chat_id.clone();
        guard.chats.insert(cid.clone(), record.metadata);
        guard.messages.insert(cid, record.messages);
    }
    debug!(
        "chat_history: loaded snapshot ({} chats)",
        guard.chats.len()
    );
    Ok(())
}

/// Persist changes after an append operation (full snapshot strategy).
pub fn persist_after_append(store: &SharedChatStore) {
    persist_snapshot(store);
}

/// Persist changes after metadata update (full snapshot strategy).
pub fn persist_after_metadata_update(store: &SharedChatStore) {
    persist_snapshot(store);
}

/// Read a value from the global key‑value store (blocking).
fn read_kvp(key: &str) -> Result<Option<String>> {
    db::smol::block_on(db::kvp::KEY_VALUE_STORE.read_kvp(key)).map_err(|e| anyhow!(e))
}

/// Write a value to the global key‑value store (blocking).
fn write_kvp(key: &str, value: String) -> Result<()> {
    db::smol::block_on(db::kvp::KEY_VALUE_STORE.write_kvp(key.to_string(), value))
        .map_err(|e| anyhow!(e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChatHistoryConfig, ChatStore, MessageRole};

    fn new_store() -> SharedChatStore {
        SharedChatStore::new(ChatStore::new(ChatHistoryConfig::default(), None))
    }

    #[test]
    fn snapshot_roundtrip() {
        let store = new_store();
        {
            let mut guard = store.lock();
            let (_meta, _msg) = guard
                .append_message(
                    None,
                    Some("proj-1".into()),
                    Some("Design".into()),
                    MessageRole::User,
                    "Initial design notes".into(),
                )
                .expect("append");
        }
        persist_snapshot(&store);

        let store2 = new_store();
        load_snapshot(&store2).expect("load snapshot success");
        let guard2 = store2.lock();
        assert_eq!(guard2.chats.len(), 1);
        let meta = guard2
            .chats
            .values()
            .next()
            .expect("metadata present")
            .clone();
        let msgs = guard2
            .messages
            .get(&meta.chat_id)
            .expect("messages present");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "Initial design notes");
    }
}
