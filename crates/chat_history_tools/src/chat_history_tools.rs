use anyhow::Result;
use async_trait::async_trait;
use chat_history::{
    ChatHistoryConfig, ChatId, ChatMessage, ChatMetadata, ChatStore, MessageRole, SharedChatStore,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Reason: Keep adapter responses structurally uniform for callers.
fn ok(value: Value) -> String {
    match value {
        Value::Object(mut map) => {
            map.insert("ok".into(), Value::Bool(true));
            Value::Object(map).to_string()
        }
        other => json!({ "ok": true, "value": other }).to_string(),
    }
}

/// Reason: Provide consistent error envelope shape.
fn err(msg: impl ToString) -> String {
    json!({ "ok": false, "error": msg.to_string() }).to_string()
}

/// User‑facing role indicator (string) converted to enum.
fn parse_role(s: &str) -> Option<MessageRole> {
    match s {
        "user" => Some(MessageRole::User),
        "assistant" => Some(MessageRole::Assistant),
        _ => None,
    }
}

/// Input for appending a message or creating a chat.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct AppendInput {
    pub chat_id: Option<String>,
    pub project_id: Option<String>,
    pub title: Option<String>,
    pub role: Option<String>,
    pub content: String,
}

/// Input for substring search.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct SearchInput {
    pub query: String,
    pub chat_id: Option<String>,
    pub top_k: Option<usize>,
}

/// Input for listing chats (pagination).
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct ListInput {
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}

/// Input for retrieving a full chat.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct GetInput {
    pub chat_id: String,
}

/// Input for simple config mutation.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct ConfigSetInput {
    pub hybrid_alpha: Option<f32>,
    pub rag_top_k: Option<usize>,
    pub summary_refresh_chars: Option<usize>,
    pub summary_delta_chars: Option<usize>,
    pub embedding_model: Option<String>,
    pub auto_tag: Option<bool>,
}

/// Projection for list operations.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct ChatSummary {
    pub chat_id: String,
    pub project_id: Option<String>,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub total_messages: usize,
    pub total_characters: usize,
    pub archived: bool,
    pub pinned: bool,
    pub tags: Vec<String>,
}

/// Tool adapter holding a shared store.
pub struct ChatHistoryTools {
    store: SharedChatStore,
}

impl ChatHistoryTools {
    pub fn new(store: ChatStore) -> Self {
        Self {
            store: SharedChatStore::new(store),
        }
    }

    /// Append helper returning metadata + message.
    fn do_append(&self, input: AppendInput) -> Result<(ChatMetadata, ChatMessage)> {
        let mut guard = self.store.lock();
        let role = input
            .role
            .as_deref()
            .and_then(parse_role)
            .unwrap_or(MessageRole::User);
        guard.append_message(
            input.chat_id.map(ChatId::new),
            input.project_id,
            input.title,
            role,
            input.content,
        )
    }

    fn do_search(&self, input: SearchInput) -> Vec<ChatMessage> {
        let guard = self.store.lock();
        let top_k = input.top_k.unwrap_or(10);
        if let Some(cid_str) = input.chat_id.as_ref() {
            let cid = ChatId::new(cid_str.clone());
            guard.search_messages(&input.query, Some(&cid), top_k)
        } else {
            guard.search_messages(&input.query, None, top_k)
        }
    }

    fn do_list(&self, input: ListInput) -> Vec<ChatSummary> {
        let guard = self.store.lock();
        let offset = input.offset.unwrap_or(0);
        let limit = input.limit.unwrap_or(20);
        guard
            .list_chats(offset, limit)
            .into_iter()
            .map(|c| ChatSummary {
                chat_id: c.chat_id.as_str().to_string(),
                project_id: c.project_id,
                title: c.title,
                summary: c.summary,
                total_messages: c.total_messages,
                total_characters: c.total_characters,
                archived: c.archived,
                pinned: c.pinned,
                tags: c.tags,
            })
            .collect()
    }

    fn do_get(&self, input: GetInput) -> Result<(ChatMetadata, Vec<ChatMessage>)> {
        let guard = self.store.lock();
        guard.get_chat(&ChatId::new(input.chat_id))
    }

    fn do_config_get(&self) -> ChatHistoryConfig {
        let guard = self.store.lock();
        guard.config().clone()
    }

    fn do_config_set(&self, input: ConfigSetInput) -> ChatHistoryConfig {
        let mut guard = self.store.lock();
        let mut cfg = guard.config().clone();
        if let Some(v) = input.hybrid_alpha {
            cfg.hybrid_alpha = v;
        }
        if let Some(v) = input.rag_top_k {
            cfg.rag_top_k = v;
        }
        if let Some(v) = input.summary_refresh_chars {
            cfg.summary_refresh_chars = v;
        }
        if let Some(v) = input.summary_delta_chars {
            cfg.summary_delta_chars = v;
        }
        if let Some(v) = input.embedding_model {
            cfg.embedding_model = Some(v);
        }
        if let Some(v) = input.auto_tag {
            cfg.auto_tag = v;
        }
        guard.set_config(cfg.clone());
        cfg
    }
}

/// Async trait so adapter can evolve to perform background work later.
#[async_trait]
pub trait ChatHistoryToolApi {
    async fn chat_append(&self, input_json: &str) -> String;
    async fn chat_search(&self, input_json: &str) -> String;
    async fn chat_list(&self, input_json: &str) -> String;
    async fn chat_get(&self, input_json: &str) -> String;
    async fn chat_config_get(&self) -> String;
    async fn chat_config_set(&self, input_json: &str) -> String;
}

#[async_trait]
impl ChatHistoryToolApi for ChatHistoryTools {
    async fn chat_append(&self, input_json: &str) -> String {
        let input: AppendInput = match serde_json::from_str(input_json) {
            Ok(v) => v,
            Err(e) => return err(format!("parse error: {e}")),
        };
        match self.do_append(input) {
            Ok((chat, message)) => ok(json!({ "chat": chat, "message": message })),
            Err(e) => err(e),
        }
    }

    async fn chat_search(&self, input_json: &str) -> String {
        let input: SearchInput = match serde_json::from_str(input_json) {
            Ok(v) => v,
            Err(e) => return err(format!("parse error: {e}")),
        };
        let matches = self.do_search(input);
        ok(json!({ "contexts": matches }))
    }

    async fn chat_list(&self, input_json: &str) -> String {
        let input: ListInput = match serde_json::from_str(input_json) {
            Ok(v) => v,
            Err(e) => return err(format!("parse error: {e}")),
        };
        ok(json!({ "chats": self.do_list(input) }))
    }

    async fn chat_get(&self, input_json: &str) -> String {
        let input: GetInput = match serde_json::from_str(input_json) {
            Ok(v) => v,
            Err(e) => return err(format!("parse error: {e}")),
        };
        match self.do_get(input) {
            Ok((meta, messages)) => ok(json!({ "chat": meta, "messages": messages })),
            Err(e) => err(e),
        }
    }

    async fn chat_config_get(&self) -> String {
        ok(json!({ "config": self.do_config_get() }))
    }

    async fn chat_config_set(&self, input_json: &str) -> String {
        let input: ConfigSetInput = match serde_json::from_str(input_json) {
            Ok(v) => v,
            Err(e) => return err(format!("parse error: {e}")),
        };
        ok(json!({ "config": self.do_config_set(input) }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn append_and_list() {
        let adapter = ChatHistoryTools::new(ChatStore::new(ChatHistoryConfig::default(), None));
        let first = adapter
            .chat_append(
                r#"{"project_id":"p","title":"T","role":"user","content":"Design phase start"}"#,
            )
            .await;
        assert!(first.contains("\"ok\":true"));
        let list = adapter.chat_list(r#"{"limit":10}"#).await;
        assert!(list.contains("\"chats\""));
    }

    #[tokio::test]
    async fn search_flow() {
        let adapter = ChatHistoryTools::new(ChatStore::new(ChatHistoryConfig::default(), None));
        adapter
            .chat_append(r#"{"content":"Alpha beta","role":"user"}"#)
            .await;
        adapter
            .chat_append(r#"{"content":"Gamma token","role":"assistant"}"#)
            .await;
        let search = adapter.chat_search(r#"{"query":"token","top_k":5}"#).await;
        assert!(search.contains("Gamma token"));
    }

    #[tokio::test]
    async fn config_set_get() {
        let adapter = ChatHistoryTools::new(ChatStore::new(ChatHistoryConfig::default(), None));
        let set = adapter.chat_config_set(r#"{"hybrid_alpha":0.7}"#).await;
        let parsed: serde_json::Value = serde_json::from_str(&set).expect("valid JSON");
        assert!(
            (parsed["config"]["hybrid_alpha"].as_f64().expect("numeric") - 0.7).abs() < 1e-6,
            "expected hybrid_alpha ≈ 0.7, got {:?}",
            parsed["config"]["hybrid_alpha"]
        );
        let get = adapter.chat_config_get().await;
        let parsed_get: serde_json::Value = serde_json::from_str(&get).expect("valid JSON");
        assert!(
            (parsed_get["config"]["hybrid_alpha"]
                .as_f64()
                .expect("numeric")
                - 0.7)
                .abs()
                < 1e-6,
            "expected hybrid_alpha ≈ 0.7, got {:?}",
            parsed_get["config"]["hybrid_alpha"]
        );
    }
}
