/// Chat history tool adapter exposing snake_case JSON interfaces.
///
/// This adapter exposes a JSON based surface over `ChatStore` suitable for LLM tool invocation.
/// It favors deterministic, schema-lite request/response envelopes.
///
/// Convention summary (informational only):
/// - Success responses include `"ok": true`.
/// - Failure responses include `"ok": false` and an `"error"` string.
/// - Inputs are JSON strings; parse failures yield error JSON.
///
/// Implemented operations (snake_case):
/// - chat_append
/// - chat_search
/// - chat_answer
/// - chat_similar
/// - chat_list
/// - chat_get
/// - chat_reembed
/// - chat_update_metadata
/// - chat_config_get
/// - chat_config_set
///
/// The caller constructs and owns the underlying `ChatStore`.
pub mod init;
pub mod migrations;

use std::sync::Arc;


use async_trait::async_trait;
use chat_history::{
    prelude::*,
    tools::{ToolAppendRequest, ToolAppendResult, ToolSearchRequest, ToolSearchResult},
    RetrievalMode,
    MessageRole,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;

// -----------------------------
// Internal request/response models
// -----------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AppendInput {
    pub chat_id: Option<String>,
    pub project_id: Option<String>,
    pub title: Option<String>,
    pub role: Option<MessageRole>,
    pub content: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SearchInput {
    pub query: String,
    pub project_id: Option<String>,
    pub chat_id: Option<String>,
    pub top_k: Option<usize>,
    pub mode: Option<RetrievalMode>,
    pub alpha: Option<f32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AnswerInput {
    pub question: String,
    pub project_id: Option<String>,
    pub chat_id: Option<String>,
    pub top_k: Option<usize>,
    pub mode: Option<RetrievalMode>,
    pub alpha: Option<f32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SimilarInput {
    pub chat_id: String,
    pub n: Option<usize>,
    pub project_scoped: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ListInput {
    pub project_id: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GetInput {
    pub chat_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ReembedInput {
    pub chat_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct UpdateMetadataInput {
    pub chat_id: String,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub tags_add: Option<Vec<String>>,
    pub tags_remove: Option<Vec<String>>,
    pub archived: Option<bool>,
    pub pinned: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ConfigSetInput {
    pub embedding_model: Option<String>,
    pub hybrid_alpha: Option<f32>,
    pub similar_chats_k: Option<usize>,
    pub summary_refresh_chars: Option<usize>,
    pub summary_delta_chars: Option<usize>,
    pub rag_top_k: Option<usize>,
    pub auto_tag: Option<bool>,
    pub default_retrieval_mode: Option<RetrievalMode>,
}

// Minimal projection for chats list
#[derive(Debug, Serialize)]
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

// -----------------------------
// Adapter
// -----------------------------



pub struct ChatHistoryTools {
    store: Arc<Mutex<ChatStore>>,
    runtime: Arc<tokio::runtime::Runtime>,
}

impl ChatHistoryTools {
    pub fn new(store: Arc<Mutex<ChatStore>>) -> Self {
        let runtime = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("chat history dedicated tokio runtime"),
        );
        Self { store, runtime }
    }

    fn err_json(msg: impl ToString) -> String {
        json!({ "ok": false, "error": msg.to_string() }).to_string()
    }

    fn ok_json(value: serde_json::Value) -> String {
        let mut obj = match value {
            serde_json::Value::Object(map) => map,
            other => {
                return json!({
                    "ok": true,
                    "value": other
                })
                .to_string()
            }
        };
        obj.insert("ok".into(), serde_json::Value::Bool(true));
        serde_json::Value::Object(obj).to_string()
    }
}

// -----------------------------
// Public async API
// -----------------------------

#[async_trait]
pub trait ChatHistoryToolApi {
    async fn chat_append(&self, input_json: &str) -> String;
    async fn chat_search(&self, input_json: &str) -> String;
    async fn chat_answer(&self, input_json: &str) -> String;
    async fn chat_similar(&self, input_json: &str) -> String;
    async fn chat_list(&self, input_json: &str) -> String;
    async fn chat_get(&self, input_json: &str) -> String;
    async fn chat_reembed(&self, input_json: &str) -> String;
    async fn chat_update_metadata(&self, input_json: &str) -> String;
    async fn chat_config_get(&self) -> String;
    async fn chat_config_set(&self, input_json: &str) -> String;
}

#[async_trait]
impl ChatHistoryToolApi for ChatHistoryTools {
    async fn chat_append(&self, input_json: &str) -> String {
        let input: AppendInput = match serde_json::from_str(input_json) {
            Ok(v) => v,
            Err(e) => return Self::err_json(format!("parse error: {e}")),
        };
        let role = input.role.unwrap_or(MessageRole::User);
        let req = ToolAppendRequest {
            chat_id: input.chat_id,
            project_id: input.project_id,
            title: input.title,
            role,
            content: input.content,
        };
        let store = self.runtime.block_on(self.store.lock());
        match self.runtime.block_on(async { store.tool_append(req).await }) {
            Ok(ToolAppendResult { chat, message }) => {
                Self::ok_json(json!({ "chat": chat, "message": message }))
            }
            Err(e) => Self::err_json(e),
        }
    }

    async fn chat_search(&self, input_json: &str) -> String {
        let input: SearchInput = match serde_json::from_str(input_json) {
            Ok(v) => v,
            Err(e) => return Self::err_json(format!("parse error: {e}")),
        };
        let req = ToolSearchRequest {
            query: input.query,
            project_id: input.project_id,
            chat_id: input.chat_id,
            top_k: input.top_k,
            mode: input.mode,
            alpha: input.alpha,
        };
        let store = self.store.lock().await;
        match store.tool_search(req).await {
            Ok(ToolSearchResult { contexts }) => {
                Self::ok_json(json!({ "contexts": contexts }))
            }
            Err(e) => Self::err_json(e),
        }
    }

    async fn chat_answer(&self, input_json: &str) -> String {
        let input: AnswerInput = match serde_json::from_str(input_json) {
            Ok(v) => v,
            Err(e) => return Self::err_json(format!("parse error: {e}")),
        };
        let store = self.store.lock().await;
        match store
            .tool_answer(
                input.question,
                input.project_id,
                input.chat_id,
                input.top_k,
                input.mode,
                input.alpha,
            )
            .await
        {
            Ok(rag) => Self::ok_json(json!({ "answer": rag.answer })),
            Err(e) => Self::err_json(e),
        }
    }

    async fn chat_similar(&self, input_json: &str) -> String {
        let input: SimilarInput = match serde_json::from_str(input_json) {
            Ok(v) => v,
            Err(e) => return Self::err_json(format!("parse error: {e}")),
        };
        let n = input.n.unwrap_or(10);
        let project_scoped = input.project_scoped.unwrap_or(true);
        let store = self.runtime.block_on(self.store.lock());
        match self.runtime.block_on(async {
            store.similar_chats(&ChatId(input.chat_id), n, project_scoped).await
        }) {
            Ok(list) => {
                let payload: Vec<_> = list
                    .into_iter()
                    .map(|(meta, score)| json!({ "chat": meta, "score": score }))
                    .collect();
                Self::ok_json(json!({ "similar": payload }))
            }
            Err(e) => Self::err_json(e),
        }
    }

    async fn chat_list(&self, input_json: &str) -> String {
        let input: ListInput = match serde_json::from_str(input_json) {
            Ok(v) => v,
            Err(e) => return Self::err_json(format!("parse error: {e}")),
        };
        let limit = input.limit.unwrap_or(20);
        let offset = input.offset.unwrap_or(0);
        let store = self.runtime.block_on(self.store.lock());
        match self.runtime.block_on(async {
            store.list_chats(input.project_id.as_deref(), limit, offset).await
        }) {
            Ok(chats) => {
                let summaries: Vec<ChatSummary> = chats
                    .into_iter()
                    .map(|c| ChatSummary {
                        chat_id: c.chat_id.0,
                        project_id: c.project_id,
                        title: c.title,
                        summary: c.summary,
                        total_messages: c.total_messages,
                        total_characters: c.total_characters,
                        archived: c.archived,
                        pinned: c.pinned,
                        tags: c.tags,
                    })
                    .collect();
                Self::ok_json(json!({ "chats": summaries }))
            }
            Err(e) => Self::err_json(e),
        }
    }

    async fn chat_get(&self, input_json: &str) -> String {
        let input: GetInput = match serde_json::from_str(input_json) {
            Ok(v) => v,
            Err(e) => return Self::err_json(format!("parse error: {e}")),
        };
        let store = self.runtime.block_on(self.store.lock());
        match self.runtime.block_on(async { store.get_chat(&ChatId(input.chat_id)).await }) {
            Ok((meta, messages)) => {
                Self::ok_json(json!({ "chat": meta, "messages": messages }))
            }
            Err(e) => Self::err_json(e),
        }
    }

    async fn chat_reembed(&self, input_json: &str) -> String {
        let input: ReembedInput = match serde_json::from_str(input_json) {
            Ok(v) => v,
            Err(e) => return Self::err_json(format!("parse error: {e}")),
        };
        let store = self.store.lock().await;
        let owned_chat_id = input.chat_id.as_ref().map(|id| ChatId(id.clone()));
        match store.reembed(owned_chat_id.as_ref()).await {
            Ok(_) => Self::ok_json(json!({ "status": "scheduled" })),
            Err(e) => Self::err_json(e),
        }
    }

    async fn chat_update_metadata(&self, input_json: &str) -> String {
        let input: UpdateMetadataInput = match serde_json::from_str(input_json) {
            Ok(v) => v,
            Err(e) => return Self::err_json(format!("parse error: {e}")),
        };
        let tags_add = input.tags_add.unwrap_or_default();
        let tags_remove = input.tags_remove.unwrap_or_default();
        let store = self.runtime.block_on(self.store.lock());
        match self.runtime.block_on(async {
            store
                .update_metadata(
                    &ChatId(input.chat_id),
                    input.title,
                    input.summary,
                    &tags_add,
                    &tags_remove,
                    input.archived,
                    input.pinned,
                )
                .await
        }) {
            Ok(updated) => Self::ok_json(json!({ "chat": updated })),
            Err(e) => Self::err_json(e),
        }
    }

    async fn chat_config_get(&self) -> String {
        let store = self.runtime.block_on(self.store.lock());
        // Clone so we can redact secrets before returning.
        let mut cfg = store.config().clone();
        if let Some(k) = cfg.openai.api_key.as_mut() {
            if !k.is_empty() {
                *k = "****".into();
            }
        }
        if let Some(k) = cfg.azure_openai.api_key.as_mut() {
            if !k.is_empty() {
                *k = "****".into();
            }
        }
        Self::ok_json(json!({ "config": cfg }))
    }

    async fn chat_config_set(&self, input_json: &str) -> String {
        let input: ConfigSetInput = match serde_json::from_str(input_json) {
            Ok(v) => v,
            Err(e) => return Self::err_json(format!("parse error: {e}")),
        };
        let mut store = self.store.lock().await;
        let mut cfg = store.config().clone();
        if let Some(v) = input.embedding_model { cfg.embedding_model = v; }
        if let Some(v) = input.hybrid_alpha { cfg.hybrid_alpha = v; }
        if let Some(v) = input.similar_chats_k { cfg.similar_chats_k = v; }
        if let Some(v) = input.summary_refresh_chars { cfg.summary_refresh_chars = v; }
        if let Some(v) = input.summary_delta_chars { cfg.summary_delta_chars = v; }
        if let Some(v) = input.rag_top_k { cfg.rag_top_k = v; }
        if let Some(v) = input.auto_tag { cfg.auto_tag = v; }
        if let Some(v) = input.default_retrieval_mode { cfg.default_retrieval_mode = v; }
        store.set_config(cfg);
        Self::ok_json(json!({ "config": store.config() }))
    }
}
