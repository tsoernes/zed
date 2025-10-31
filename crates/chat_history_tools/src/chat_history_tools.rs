use anyhow::Result;
use async_trait::async_trait;
use chat_history::{
    ChatHistoryConfig, ChatHistoryConfigPatch, ChatId, ChatMessage, ChatMetadata, ChatStore,
    MessageRole, RetrievalMode, SearchHit, SharedChatStore, SimilarChat,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
// unused import removed

/* WHY: Uniform success envelope required by tools spec */
fn ok(value: Value) -> String {
    match value {
        Value::Object(mut map) => {
            map.insert("ok".into(), Value::Bool(true));
            Value::Object(map).to_string()
        }
        other => json!({ "ok": true, "value": other }).to_string(),
    }
}

/* WHY: Uniform error envelope shape for predictable parsing */
fn err(msg: impl ToString) -> String {
    json!({ "ok": false, "error": msg.to_string() }).to_string()
}

/* WHY: Explicit parsing to allow tolerant casing & future variants */
fn parse_role(s: &str) -> Option<MessageRole> {
    match s.to_ascii_lowercase().as_str() {
        "user" => Some(MessageRole::User),
        "assistant" => Some(MessageRole::Assistant),
        _ => None,
    }
}

/* WHY: Retrieval mode string mapping (case & mixed forms) */
fn parse_retrieval_mode(s: &str) -> Option<RetrievalMode> {
    match s.to_ascii_lowercase().as_str() {
        "bm25" => Some(RetrievalMode::Bm25),
        "embedding" => Some(RetrievalMode::Embedding),
        "hybrid" => Some(RetrievalMode::Hybrid),
        _ => None,
    }
}

/* ============ Input Structs ============ */

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct AppendInput {
    pub chat_id: Option<String>,
    pub project_id: Option<String>,
    pub title: Option<String>,
    pub role: Option<String>,
    pub content: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct SearchInput {
    pub query: String,
    pub project_id: Option<String>,
    pub chat_id: Option<String>,
    pub top_k: Option<usize>,
    pub mode: Option<String>,
    pub alpha: Option<f32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct AnswerInput {
    pub question: String,
    pub project_id: Option<String>,
    pub chat_id: Option<String>,
    pub top_k: Option<usize>,
    pub mode: Option<String>,
    pub alpha: Option<f32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct ListInput {
    pub project_id: Option<String>,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct GetInput {
    pub chat_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct SimilarInput {
    pub chat_id: String,
    pub n: Option<usize>,
    pub project_scoped: Option<bool>, // currently unused (project scope unsupported in core)
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct ReembedInput {
    pub chat_id: Option<String>,
    pub force: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
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

/* WHY: Config setter allows sparse patch overlay; secrets may be present */
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct ConfigSetInput {
    pub embedding_model: Option<String>,
    pub hybrid_alpha: Option<f32>,
    pub similar_chats_k: Option<usize>,
    pub summary_refresh_chars: Option<usize>,
    pub summary_delta_chars: Option<usize>,
    pub rag_top_k: Option<usize>,
    pub auto_tag: Option<bool>,
    pub default_retrieval_mode: Option<String>,
    pub openai_api_key: Option<String>,
    pub openai_api_url: Option<String>,
    pub openai_model: Option<String>,
    pub openai_embedding_model: Option<String>,
    pub azure_api_key: Option<String>,
    pub azure_endpoint: Option<String>,
    pub azure_api_version: Option<String>,
    pub azure_deployment: Option<String>,
    pub azure_embedding_model: Option<String>,
}

/* ============ Output Projection Structs ============ */

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

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct ContextHit {
    chat_id: String,
    message_id: String,
    score: f32,
    content: String,
    role: MessageRole,
}

/* ============ Adapter ============ */

pub struct ChatHistoryTools {
    store: SharedChatStore,
}

impl ChatHistoryTools {
    pub fn new(store: ChatStore) -> Self {
        Self {
            store: SharedChatStore::new(store),
        }
    }

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

    fn do_list(&self, input: ListInput) -> Vec<ChatSummary> {
        let guard = self.store.lock();
        let offset = input.offset.unwrap_or(0);
        let limit = input.limit.unwrap_or(20);
        guard
            .list_chats(offset, limit)
            .into_iter()
            .filter(|c| {
                if let Some(ref pid) = input.project_id {
                    c.project_id.as_ref() == Some(pid)
                } else {
                    true
                }
            })
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
        Ok(guard.get_chat(&ChatId::new(input.chat_id))?)
    }

    fn do_update_metadata(&self, input: UpdateMetadataInput) -> Result<ChatMetadata> {
        let guard = self.store.lock();
        let cid = ChatId::new(&input.chat_id);
        let (meta, _msgs) = guard.get_chat(&cid)?;
        let mut meta = meta;
        if let Some(t) = input.title {
            meta.title = Some(t);
        }
        if let Some(s) = input.summary {
            meta.summary = Some(s);
        }
        if let Some(a) = input.archived {
            meta.archived = a;
        }
        if let Some(p) = input.pinned {
            meta.pinned = p;
        }
        if let Some(add) = input.tags_add {
            for tag in add {
                if !meta.tags.iter().any(|t| t == &tag) {
                    meta.tags.push(tag);
                }
            }
        }
        if let Some(remove) = input.tags_remove {
            meta.tags.retain(|t| !remove.iter().any(|r| r == t));
        }
        // TODO: persist metadata update once ChatStore exposes a public setter
        Ok(meta)
    }

    fn redact_config(cfg: &ChatHistoryConfig) -> Value {
        fn redact(opt: &Option<String>) -> Option<String> {
            opt.as_ref().map(|s| {
                if s.is_empty() {
                    "".into()
                } else {
                    "****".into()
                }
            })
        }
        json!({
          "embedding_model": cfg.embedding_model,
          "hybrid_alpha": cfg.hybrid_alpha,
          "similar_chats_k": cfg.similar_chats_k,
          "summary_refresh_chars": cfg.summary_refresh_chars,
          "summary_delta_chars": cfg.summary_delta_chars,
          "rag_top_k": cfg.rag_top_k,
          "auto_tag": cfg.auto_tag,
          "default_retrieval_mode": format!("{:?}", cfg.default_retrieval_mode),
          "openai": cfg.openai.as_ref().map(|o| json!({
              "api_key": redact(&o.api_key),
              "api_url": o.api_url,
              "model": o.model,
              "embedding_model": o.embedding_model
          })),
          "azure_openai": cfg.azure_openai.as_ref().map(|a| json!({
              "api_key": redact(&a.api_key),
              "endpoint": a.endpoint,
              "api_version": a.api_version,
              "deployment": a.deployment,
              "embedding_model": a.embedding_model
          }))
        })
    }

    fn apply_config_patch(&self, input: ConfigSetInput) -> ChatHistoryConfig {
        let mut guard = self.store.lock();
        let mut patch = ChatHistoryConfigPatch::default();
        patch.embedding_model = input.embedding_model;
        patch.hybrid_alpha = input.hybrid_alpha;
        patch.similar_chats_k = input.similar_chats_k;
        patch.summary_refresh_chars = input.summary_refresh_chars;
        patch.summary_delta_chars = input.summary_delta_chars;
        patch.rag_top_k = input.rag_top_k;
        patch.auto_tag = input.auto_tag;
        if let Some(mode_s) = input.default_retrieval_mode {
            if let Some(m) = parse_retrieval_mode(&mode_s) {
                patch.default_retrieval_mode = Some(m);
            }
        }
        if input.openai_api_key.is_some()
            || input.openai_api_url.is_some()
            || input.openai_model.is_some()
            || input.openai_embedding_model.is_some()
        {
            patch.openai = Some(chat_history::OpenAIConfig {
                api_key: input.openai_api_key,
                api_url: input.openai_api_url,
                model: input.openai_model,
                embedding_model: input.openai_embedding_model,
            });
        }
        if input.azure_api_key.is_some()
            || input.azure_endpoint.is_some()
            || input.azure_api_version.is_some()
            || input.azure_deployment.is_some()
            || input.azure_embedding_model.is_some()
        {
            patch.azure_openai = Some(chat_history::AzureOpenAIConfig {
                api_key: input.azure_api_key,
                endpoint: input.azure_endpoint,
                api_version: input.azure_api_version,
                deployment: input.azure_deployment,
                embedding_model: input.azure_embedding_model,
            });
        }
        guard.update_config(patch).clone()
    }

    fn build_context_hits(
        &self,
        hits: Vec<SearchHit>,
        top_k: usize,
        project_id_filter: Option<String>,
    ) -> Vec<ContextHit> {
        let guard = self.store.lock();
        hits.into_iter()
            .filter(|h| {
                if let Some(ref pid) = project_id_filter {
                    if let Ok((meta, _)) = guard.get_chat(&h.chat_id) {
                        return meta.project_id.as_ref() == Some(pid);
                    } else {
                        return false;
                    }
                }
                true
            })
            .take(top_k)
            .map(|h| {
                let message_id = serde_json::to_value(&h.message_id)
                    .ok()
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .unwrap_or_default();
                ContextHit {
                    chat_id: h.chat_id.as_str().to_string(),
                    message_id,
                    score: h.fused_score,
                    content: h.content,
                    role: h.role,
                }
            })
            .collect()
    }
}

/* ============ Tool API Trait ============ */

#[async_trait]
pub trait ChatHistoryToolApi {
    async fn chat_append(&self, input_json: &str) -> String;
    async fn chat_search(&self, input_json: &str) -> String;
    async fn chat_answer(&self, input_json: &str) -> String;
    async fn chat_list(&self, input_json: &str) -> String;
    async fn chat_get(&self, input_json: &str) -> String;
    async fn chat_similar(&self, input_json: &str) -> String;
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
        let top_k = input.top_k.unwrap_or(20).max(1);
        // Hybrid retrieval path
        let (mode, alpha) = (
            input.mode.as_deref().and_then(parse_retrieval_mode),
            input.alpha,
        );
        let guard = self.store.lock();
        let chat_id_opt = input.chat_id.as_ref().map(|c| ChatId::new(c.clone()));
        let hits = guard.hybrid_search(&input.query, chat_id_opt.as_ref(), top_k, mode, alpha);
        drop(guard);
        match hits {
            Ok(h) => {
                let contexts = self.build_context_hits(h, top_k, input.project_id.clone());
                ok(json!({ "contexts": contexts }))
            }
            Err(e) => err(e),
        }
    }

    async fn chat_answer(&self, input_json: &str) -> String {
        let input: AnswerInput = match serde_json::from_str(input_json) {
            Ok(v) => v,
            Err(e) => return err(format!("parse error: {e}")),
        };
        let top_k = input.top_k.unwrap_or(6).max(1);
        let guard = self.store.lock();
        let chat_id_opt = input.chat_id.as_ref().map(|c| ChatId::new(c.clone()));
        let hits = guard.hybrid_search(
            &input.question,
            chat_id_opt.as_ref(),
            top_k,
            input.mode.as_deref().and_then(parse_retrieval_mode),
            input.alpha,
        );
        drop(guard);
        match hits {
            Ok(h) => {
                let contexts = self.build_context_hits(h, top_k, input.project_id.clone());
                // Stub answer strategy (placeholder)
                let answer = if contexts.is_empty() {
                    "No relevant context found.".to_string()
                } else {
                    format!(
                        "Derived answer referencing {} context fragment(s).",
                        contexts.len()
                    )
                };
                ok(json!({ "answer": answer, "contexts_used": contexts.len() }))
            }
            Err(e) => err(e),
        }
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

    async fn chat_similar(&self, input_json: &str) -> String {
        let input: SimilarInput = match serde_json::from_str(input_json) {
            Ok(v) => v,
            Err(e) => return err(format!("parse error: {e}")),
        };
        let cid = ChatId::new(&input.chat_id);
        let guard = self.store.lock();
        let result = guard.similar_chats(&cid, input.n);
        drop(guard);
        match result {
            Ok(sims) => {
                let payload: Vec<Value> = sims
                    .into_iter()
                    .map(|s: SimilarChat| {
                        json!({
                            "chat": s.chat,
                            "score": s.score
                        })
                    })
                    .collect();
                ok(json!({ "similar": payload }))
            }
            Err(e) => err(e),
        }
    }

    async fn chat_reembed(&self, input_json: &str) -> String {
        let input: ReembedInput = match serde_json::from_str(input_json) {
            Ok(v) => v,
            Err(e) => return err(format!("parse error: {e}")),
        };
        let mut guard = self.store.lock();
        let chat_id_opt = input.chat_id.as_ref().map(|c| ChatId::new(c.clone()));
        let r = guard.reembed(chat_id_opt.as_ref(), input.force.unwrap_or(false));
        match r {
            Ok(_) => ok(json!({ "status": "scheduled" })),
            Err(e) => err(e),
        }
    }

    async fn chat_update_metadata(&self, input_json: &str) -> String {
        let input: UpdateMetadataInput = match serde_json::from_str(input_json) {
            Ok(v) => v,
            Err(e) => return err(format!("parse error: {e}")),
        };
        match self.do_update_metadata(input) {
            Ok(chat) => ok(json!({ "chat": chat })),
            Err(e) => err(e),
        }
    }

    async fn chat_config_get(&self) -> String {
        let guard = self.store.lock();
        let cfg = guard.config().clone();
        drop(guard);
        ok(json!({ "config": Self::redact_config(&cfg) }))
    }

    async fn chat_config_set(&self, input_json: &str) -> String {
        let input: ConfigSetInput = match serde_json::from_str(input_json) {
            Ok(v) => v,
            Err(e) => return err(format!("parse error: {e}")),
        };
        let cfg = self.apply_config_patch(input);
        ok(json!({ "config": Self::redact_config(&cfg) }))
    }
}

/* ============ Tests ============ */

#[cfg(test)]
mod tests {
    use super::*;
    use chat_history::ChatHistoryConfig;
    use tokio;

    #[tokio::test]
    async fn append_search_roundtrip() {
        let adapter = ChatHistoryTools::new(ChatStore::new(ChatHistoryConfig::default(), None));
        let res = adapter
            .chat_append(
                r#"{"project_id":"p1","title":"T","role":"user","content":"Initial design baseline"}"#,
            )
            .await;
        assert!(res.contains("\"ok\":true"));
        let search = adapter
            .chat_search(r#"{"query":"baseline","top_k":5,"mode":"bm25"}"#)
            .await;
        assert!(search.contains("baseline"));
    }

    #[tokio::test]
    async fn config_patch_and_redaction() {
        let adapter = ChatHistoryTools::new(ChatStore::new(ChatHistoryConfig::default(), None));
        adapter
            .chat_config_set(
                r#"{"hybrid_alpha":0.7,"openai_api_key":"sk-test","default_retrieval_mode":"embedding"}"#,
            )
            .await;
        let cfg = adapter.chat_config_get().await;
        let cfg_json: serde_json::Value = serde_json::from_str(&cfg).expect("valid json");
        assert!(
            (cfg_json["config"]["hybrid_alpha"].as_f64().unwrap() - 0.7).abs() < 1e-9,
            "expected hybrid_alpha 0.7, got {:?}",
            cfg_json["config"]["hybrid_alpha"]
        );
        assert!(cfg.contains("\"api_key\":\"****\""));
    }

    #[tokio::test]
    async fn update_metadata_and_list() {
        let adapter = ChatHistoryTools::new(ChatStore::new(ChatHistoryConfig::default(), None));
        let appended = adapter
            .chat_append(r#"{"content":"A note about latency","role":"user"}"#)
            .await;
        let parsed: Value = serde_json::from_str(&appended).unwrap();
        let chat_id = parsed["chat"]["chat_id"].as_str().unwrap();
        let upd = adapter
            .chat_update_metadata(&format!(
                r#"{{"chat_id":"{chat_id}","title":"Latency Discussion","tags_add":["perf"]}}"#
            ))
            .await;
        assert!(upd.contains("Latency Discussion"));
        let listed = adapter.chat_list(r#"{"limit":10}"#).await;
        let fetched = adapter
            .chat_get(&format!(r#"{{"chat_id":"{chat_id}"}}"#))
            .await;
        assert!(
            fetched.contains("Latency Discussion"),
            "updated title not reflected in chat_get response: {fetched}"
        );
    }
}
