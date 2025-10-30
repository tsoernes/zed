use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Identifier for a chat.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct ChatId(String);

impl ChatId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
    /// Expose the underlying string slice (needed by external adapters without
    /// granting mutable access).
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Identifier for a message.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct MessageId(String);

impl MessageId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

/// Role of a message in the conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
}

/// A single message stored in chat history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub message_id: MessageId,
    pub chat_id: ChatId,
    pub role: MessageRole,
    pub content: String,
    pub created_at: DateTime<Utc>,
    pub token_estimate: usize,
}

/// Metadata summarizing a chat.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMetadata {
    pub chat_id: ChatId,
    pub project_id: Option<String>,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub total_messages: usize,
    pub total_characters: usize,
    pub token_estimate: usize,
    pub archived: bool,
    pub pinned: bool,
    pub tags: Vec<String>,
    pub embedding_model: Option<String>,
}

/// Configuration controlling retrieval and summarization thresholds.
/// Values kept intentionally minimal.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ChatHistoryConfig {
    pub hybrid_alpha: f32,
    pub rag_top_k: usize,
    pub summary_refresh_chars: usize,
    pub summary_delta_chars: usize,
    pub embedding_model: Option<String>,
    pub auto_tag: bool,
}

impl Default for ChatHistoryConfig {
    fn default() -> Self {
        Self {
            hybrid_alpha: 0.55,
            rag_top_k: 6,
            summary_refresh_chars: 4000,
            summary_delta_chars: 1500,
            embedding_model: None,
            auto_tag: true,
        }
    }
}

/// An embedding backend concept (placeholder).
pub trait EmbeddingBackend: Send + Sync {
    fn model_name(&self) -> &str;
    fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(Vec::new())
    }
}

/// In-memory store for chats and messages.
/// Intended to be wrapped inside a tool adapter; concurrency achieved via external locking.
pub struct ChatStore {
    config: ChatHistoryConfig,
    chats: HashMap<ChatId, ChatMetadata>,
    messages: HashMap<ChatId, Vec<ChatMessage>>,
    embedding_backend: Option<Arc<dyn EmbeddingBackend>>,
}

impl ChatStore {
    pub fn new(
        config: ChatHistoryConfig,
        embedding_backend: Option<Arc<dyn EmbeddingBackend>>,
    ) -> Self {
        Self {
            config,
            chats: HashMap::new(),
            messages: HashMap::new(),
            embedding_backend,
        }
    }

    pub fn config(&self) -> &ChatHistoryConfig {
        &self.config
    }

    pub fn set_config(&mut self, new_cfg: ChatHistoryConfig) {
        self.config = new_cfg;
    }

    /// Append a message, creating the chat if necessary.
    pub fn append_message(
        &mut self,
        chat_id: Option<ChatId>,
        project_id: Option<String>,
        title: Option<String>,
        role: MessageRole,
        content: String,
    ) -> Result<(ChatMetadata, ChatMessage)> {
        let now = Utc::now();
        let cid = chat_id.unwrap_or_else(|| ChatId::new(uuid::Uuid::new_v4().to_string()));
        let message_id = MessageId::new(uuid::Uuid::new_v4().to_string());
        let token_estimate = Self::estimate_tokens(&content);

        if !self.chats.contains_key(&cid) {
            let meta = ChatMetadata {
                chat_id: cid.clone(),
                project_id,
                title,
                summary: None,
                created_at: now,
                updated_at: now,
                total_messages: 0,
                total_characters: 0,
                token_estimate: 0,
                archived: false,
                pinned: false,
                tags: Vec::new(),
                embedding_model: self
                    .embedding_backend
                    .as_ref()
                    .map(|b| b.model_name().to_string()),
            };
            self.chats.insert(cid.clone(), meta);
            self.messages.insert(cid.clone(), Vec::new());
        }

        let message = ChatMessage {
            message_id,
            chat_id: cid.clone(),
            role,
            content,
            created_at: now,
            token_estimate,
        };

        let msgs = self
            .messages
            .get_mut(&cid)
            .ok_or_else(|| anyhow!("chat messages container missing"))?;
        msgs.push(message.clone());

        let meta = self
            .chats
            .get_mut(&cid)
            .ok_or_else(|| anyhow!("chat metadata missing"))?;

        meta.total_messages = msgs.len();
        meta.total_characters += message.content.len();
        meta.token_estimate += message.token_estimate;
        meta.updated_at = now;

        // Summary refresh placeholder decision (actual summarization omitted).
        if Self::should_refresh_summary(meta, &self.config) {
            // Leave summary untouched for conceptual skeleton.
        }

        Ok((meta.clone(), message))
    }

    /// List chat metadata (pagination).
    pub fn list_chats(&self, offset: usize, limit: usize) -> Vec<ChatMetadata> {
        self.chats
            .values()
            .skip(offset)
            .take(limit)
            .cloned()
            .collect()
    }

    /// Retrieve a single chat with its messages.
    pub fn get_chat(&self, chat_id: &ChatId) -> Result<(ChatMetadata, Vec<ChatMessage>)> {
        let meta = self
            .chats
            .get(chat_id)
            .ok_or_else(|| anyhow!("chat not found: {}", chat_id.0))?
            .clone();
        let msgs = self
            .messages
            .get(chat_id)
            .map(|v| v.clone())
            .unwrap_or_default();
        Ok((meta, msgs))
    }

    /// Placeholder search: returns recent messages containing the query substring (case insensitive).
    pub fn search_messages(
        &self,
        query: &str,
        chat_id: Option<&ChatId>,
        top_k: usize,
    ) -> Vec<ChatMessage> {
        let needle = query.to_lowercase();
        let mut candidates = Vec::new();

        let sources: Box<dyn Iterator<Item = (&ChatId, &Vec<ChatMessage>)>> =
            if let Some(cid) = chat_id {
                match self.messages.get(cid) {
                    Some(v) => Box::new(std::iter::once((cid, v))),
                    None => Box::new(std::iter::empty()),
                }
            } else {
                Box::new(self.messages.iter())
            };

        for (_cid, msgs) in sources {
            for msg in msgs.iter().rev() {
                if msg.content.to_lowercase().contains(&needle) {
                    candidates.push(msg.clone());
                    if candidates.len() >= top_k {
                        return candidates;
                    }
                }
            }
        }
        candidates
    }

    fn estimate_tokens(text: &str) -> usize {
        // Heuristic token approximation: average 4 chars per token for English-like text.
        (text.len() / 4).max(1)
    }

    fn should_refresh_summary(meta: &ChatMetadata, cfg: &ChatHistoryConfig) -> bool {
        if meta.total_characters < cfg.summary_refresh_chars {
            return false;
        }
        // Without tracking last summary size, treat entire size as delta for initial conceptual pass.
        meta.total_characters >= cfg.summary_refresh_chars + cfg.summary_delta_chars
    }
}

/// Thread-safe wrapper for ChatStore (optional).
#[derive(Clone)]
pub struct SharedChatStore(Arc<Mutex<ChatStore>>);

impl SharedChatStore {
    pub fn new(store: ChatStore) -> Self {
        Self(Arc::new(Mutex::new(store)))
    }

    pub fn lock(&self) -> parking_lot::MutexGuard<'_, ChatStore> {
        self.0.lock()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_and_get() {
        let mut store = ChatStore::new(ChatHistoryConfig::default(), None);
        let (meta, msg) = store
            .append_message(
                None,
                Some("proj".into()),
                Some("Title".into()),
                MessageRole::User,
                "Hello world".into(),
            )
            .expect("append");
        assert_eq!(meta.total_messages, 1);
        assert_eq!(msg.content, "Hello world");
        let (meta2, msgs) = store.get_chat(&meta.chat_id).expect("get");
        assert_eq!(meta2.chat_id, meta.chat_id);
        assert_eq!(msgs.len(), 1);
    }

    #[test]
    fn search_substring() {
        let mut store = ChatStore::new(ChatHistoryConfig::default(), None);
        let cid = ChatId::new("c1");
        store
            .append_message(
                Some(cid.clone()),
                None,
                None,
                MessageRole::User,
                "Rust retrieval design".into(),
            )
            .unwrap();
        store
            .append_message(
                Some(cid.clone()),
                None,
                None,
                MessageRole::Assistant,
                "Discussed token estimation heuristics".into(),
            )
            .unwrap();

        let results = store.search_messages("token", Some(&cid), 5);
        assert_eq!(results.len(), 1);
        assert!(results[0].content.contains("token"));
    }
}
