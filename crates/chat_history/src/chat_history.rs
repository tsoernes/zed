//! Chat history persistence, retrieval, similarity search, and RAG over prior conversations.
pub mod fastembed_cache;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_settings_apply_basic_fields() {
        let root = json!({
            "chat_history": {
                "embedding_model": "custom-model",
                "hybrid_alpha": 0.9,
                "similar_chats_k": 7,
                "summary_refresh_chars": 1234,
                "summary_delta_chars": 321,
                "rag_top_k": 4,
                "auto_tag": false,
                "default_retrieval_mode": "bm25"
            }
        });
        let cfg = ChatHistoryConfig::from_settings_root(&root);
        assert_eq!(cfg.embedding_model, "custom-model");
        assert_eq!(cfg.hybrid_alpha, 0.9_f32);
        assert_eq!(cfg.similar_chats_k, 7);
        assert_eq!(cfg.summary_refresh_chars, 1234);
        assert_eq!(cfg.summary_delta_chars, 321);
        assert_eq!(cfg.rag_top_k, 4);
        assert!(!cfg.auto_tag);
        matches!(cfg.default_retrieval_mode, RetrievalMode::Bm25);
    }

    #[test]
    fn test_settings_openai_and_backend_switch() {
        let root = json!({
            "chat_history": {
                "embedding_backend": "openai",
                "openai": {
                    "api_key": "sk-123",
                    "model": "text-embedding-3-large"
                }
            }
        });
        let cfg = ChatHistoryConfig::from_settings_root(&root);
        if let EmbeddingBackendKind::OpenAI { api_key, model } = cfg.embedding_backend {
            assert_eq!(api_key, "sk-123");
            assert_eq!(model, "text-embedding-3-large");
        } else {
            panic!("expected openai backend");
        }
        assert_eq!(cfg.openai.api_key.as_deref(), Some("sk-123"));
        assert_eq!(cfg.openai.model.as_deref(), Some("text-embedding-3-large"));
    }

    #[test]
    fn test_settings_azure_with_embedding_model() {
        let root = json!({
            "chat_history": {
                "embedding_backend": "azure_openai",
                "azure_openai": {
                    "api_key": "az-abc",
                    "endpoint": "https://example.openai.azure.com",
                    "api_version": "2024-02-15-preview",
                    "deployment": "embed-prod",
                    "embedding_model": "azure-embed-1"
                }
            }
        });
        let cfg = ChatHistoryConfig::from_settings_root(&root);
        assert_eq!(cfg.azure_openai.api_key.as_deref(), Some("az-abc"));
        assert_eq!(
            cfg.azure_openai.endpoint.as_deref(),
            Some("https://example.openai.azure.com")
        );
        assert_eq!(
            cfg.azure_openai.api_version.as_deref(),
            Some("2024-02-15-preview")
        );
        assert_eq!(cfg.azure_openai.deployment.as_deref(), Some("embed-prod"));
        assert_eq!(
            cfg.azure_openai.embedding_model.as_deref(),
            Some("azure-embed-1")
        );
        match cfg.embedding_backend {
            EmbeddingBackendKind::AzureOpenAI {
                api_key,
                endpoint,
                model,
            } => {
                assert_eq!(api_key, "az-abc");
                assert_eq!(endpoint, "https://example.openai.azure.com");
                assert_eq!(model, "azure-embed-1");
            }
            _ => panic!("expected azure backend"),
        }
    }

    #[test]
    fn test_backend_reconciliation_after_partial_keys() {
        let root = json!({
            "chat_history": {
                "embedding_backend": "openai",
                "openai": {
                    "api_key": "sk-xyz"
                }
            }
        });
        let mut cfg = ChatHistoryConfig::from_settings_root(&root);
        // simulate adding a model later
        cfg.openai.model = Some("later-model".into());
        cfg.reconcile_backend_variant();
        match cfg.embedding_backend {
            EmbeddingBackendKind::OpenAI { api_key, model } => {
                assert_eq!(api_key, "sk-xyz");
                assert_eq!(model, "later-model");
            }
            _ => panic!("expected openai backend"),
        }
    }

    // Live (optional) Azure embedding smoke test. Skips if required env vars are absent.
    // Required env vars: AZURE_OPENAI_API_KEY, AZURE_OPENAI_ENDPOINT (or AZURE_ENDPOINT),
    // AZURE_OPENAI_API_VERSION (or AZURE_API_VERSION), AZURE_OPENAI_EMBEDDING_DEPLOYMENT (or AZURE_EMBEDDING_DEPLOYMENT)
    #[tokio::test]
    async fn azure_embeddings_live_smoke() -> anyhow::Result<()> {
        let key = std::env::var("AZURE_OPENAI_API_KEY").ok();
        let endpoint = std::env::var("AZURE_OPENAI_ENDPOINT")
            .ok()
            .or_else(|| std::env::var("AZURE_ENDPOINT").ok());
        let version = std::env::var("AZURE_OPENAI_API_VERSION")
            .ok()
            .or_else(|| std::env::var("AZURE_API_VERSION").ok());
        let deployment = std::env::var("AZURE_OPENAI_EMBEDDING_DEPLOYMENT")
            .ok()
            .or_else(|| std::env::var("AZURE_EMBEDDING_DEPLOYMENT").ok());
        if key.is_none() || endpoint.is_none() || version.is_none() || deployment.is_none() {
            eprintln!("azure embedding env vars not fully set; skipping live test");
            return Ok(());
        }
        let backend = AzureOpenAIEmbeddingBackend::new_with_config(
            deployment.clone().unwrap(),
            endpoint.unwrap(),
            key.unwrap(),
            version.unwrap(),
            deployment.unwrap(),
        );
        let inputs = vec!["Hello world".to_string(), "Another sentence".to_string()];
        let vectors = backend.embed(&inputs).await?;
        assert_eq!(vectors.len(), inputs.len());
        assert!(vectors.iter().all(|v| !v.is_empty()));
        let dim = vectors[0].len();
        assert!(dim > 0);
        assert!(vectors.iter().all(|v| v.len() == dim));
        Ok(())
    }
}
//
// Internal note: summary refresh & tag suggestion logic has been integrated.
// Added `prelude` module for convenient bulk import of common types.
pub mod prelude {
    pub use crate::{
        ChatHistoryConfig, ChatId, ChatMessage, ChatMetadata, ChatStore, EmbeddingBackend,
        EmbeddingBackendKind, InMemoryIndex, MessageId, MessageRole, RagAnswer, RetrievedContext,
        fuse_scores, normalize_keyword_score, pool_chat_embedding,
    };
}
pub mod db;
pub mod entities;
// Removed duplicate `pub mod tools;` declaration—inline tools module follows.

// Tool module providing lightweight, serializable request/response structures for external agent usage.
pub mod tools {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    pub struct ToolSearchRequest {
        pub query: String,
        pub project_id: Option<String>,
        pub chat_id: Option<String>,
        pub top_k: Option<usize>,
        pub mode: Option<RetrievalMode>,
        pub alpha: Option<f32>,
    }

    #[derive(Serialize, Deserialize)]
    pub struct ToolSearchResult {
        pub contexts: Vec<RetrievedContext>,
    }

    #[derive(Serialize, Deserialize)]
    pub struct ToolAppendRequest {
        pub chat_id: Option<String>,
        pub project_id: Option<String>,
        pub title: Option<String>,
        pub role: MessageRole,
        pub content: String,
    }

    #[derive(Serialize, Deserialize)]
    pub struct ToolAppendResult {
        pub chat: ChatMetadata,
        pub message: ChatMessage,
    }
}
// Skeleton implementation. Functions return `anyhow::Result` and many are currently stubs so
// downstream crates can start wiring without failures.
//
// Persistence backend (DB vs file) and indexing will be added in subsequent iterations.

use crate::db::ChatHistoryDb;
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fs, sync::Arc};
use time::OffsetDateTime;

/// Unique identifier for a chat.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ChatId(pub String);

/// Unique identifier for a message.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct MessageId(pub String);

/// Role of a message inside a chat.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Tool,
    Other(String),
}

/// Individual message with minimal metadata.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: MessageId,
    pub chat_id: ChatId,
    pub role: MessageRole,
    pub content: String,
    pub created_at: OffsetDateTime,
    pub token_estimate: usize,
    pub tags: Vec<String>,
    /// Hash/digest of normalized content (for embedding reuse).
    pub embedding_digest: Option<Vec<u8>>,
}

/// Metadata for a chat, including aggregated stats.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatMetadata {
    pub chat_id: ChatId,
    pub project_id: Option<String>,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub total_messages: usize,
    pub total_characters: usize,
    /// Character count at which the last summary refresh occurred.
    pub summary_refreshed_characters: usize,
    pub token_estimate: usize,
    pub embedding_model: Option<String>,
    pub archived: bool,
    pub pinned: bool,
    pub tags: Vec<String>,
    /// Cached pooled embedding for the entire chat (e.g., mean of message embeddings).
    pub chat_vector: Option<Vec<f32>>,
}

/// Retrieved context snippet for RAG or search result.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RetrievedContext {
    pub chat_id: ChatId,
    pub message_id: MessageId,
    pub role: MessageRole,
    pub content_excerpt: String,
    pub score: f32,
}

/// Final RAG answer plus provenance.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RagAnswer {
    pub answer: String,
    pub contexts: Vec<RetrievedContext>,
}

/// Configuration values (will be integrated with the workspace settings system).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatHistoryConfig {
    pub default_retrieval_mode: RetrievalMode,
    pub embedding_backend: EmbeddingBackendKind,
    pub embedding_model: String,
    pub hybrid_alpha: f32,
    pub similar_chats_k: usize,
    /// Character threshold for triggering summary refresh.
    pub summary_refresh_chars: usize,
    /// Character delta required since last summary to trigger refresh.
    pub summary_delta_chars: usize,
    /// Maximum number of contexts to retrieve for RAG.
    pub rag_top_k: usize,
    /// Whether automatic tag suggestion is enabled.
    pub auto_tag: bool,
    /// Interval (message count) for full chat vector recompute (otherwise incremental update).
    pub chat_vector_recompute_interval: usize,
    /// OpenAI-specific configuration (overrides embedding_model when active).
    pub openai: OpenAiConfig,
    /// Azure OpenAI-specific configuration (endpoint, api_version, deployment, etc.).
    pub azure_openai: AzureOpenAiConfig,
    // NOTE: persisted in editor settings.json under key: `chat_history`
    // Fields: embedding_backend, embedding_model, hybrid_alpha, similar_chats_k,
    // summary_refresh_chars, summary_delta_chars, rag_top_k, auto_tag, chat_vector_recompute_interval,
    // default_retrieval_mode, openai, azure_openai
}

/// OpenAI configuration; all fields optional so they can be layered from settings.json and UI.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct OpenAiConfig {
    pub api_key: Option<String>,
    pub api_url: Option<String>,
    /// Preferred embedding model (e.g. "text-embedding-3-small"); overrides ChatHistoryConfig.embedding_model when present.
    pub model: Option<String>,
}

/// Azure OpenAI configuration.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct AzureOpenAiConfig {
    pub api_key: Option<String>,
    pub endpoint: Option<String>,
    pub api_version: Option<String>,
    /// Deployment / resource name (sometimes called "deployment" or "engine").
    pub deployment: Option<String>,
    /// Embedding model name if distinct from deployment.
    pub embedding_model: Option<String>,
}

/// Default configuration; loaded / overridden from editor settings.json (`chat_history` section).
impl Default for ChatHistoryConfig {
    fn default() -> Self {
        Self {
            embedding_backend: EmbeddingBackendKind::FastEmbedLocal { model_path: None },
            embedding_model: "bge-base-en-v1.5".into(),
            hybrid_alpha: 0.55,
            default_retrieval_mode: RetrievalMode::Hybrid,
            similar_chats_k: 10,
            summary_refresh_chars: 4_000,
            summary_delta_chars: 1_500,
            rag_top_k: 6,
            auto_tag: true,
            chat_vector_recompute_interval: 30,
            openai: OpenAiConfig::default(),
            azure_openai: AzureOpenAiConfig::default(),
        }
    }
}

impl ChatHistoryConfig {
    /// Merge a `chat_history` settings object (already the nested object) into this config.
    /// Unknown fields are ignored. This is intentionally forgiving to allow forward-compatible keys.
    pub fn apply_settings_object(&mut self, value: &serde_json::Value) {
        if let Some(mode) = value.get("default_retrieval_mode").and_then(|v| v.as_str()) {
            if let Some(parsed) = match mode {
                "bm25" => Some(RetrievalMode::Bm25),
                "embedding" => Some(RetrievalMode::Embedding),
                "hybrid" => Some(RetrievalMode::Hybrid),
                _ => None,
            } {
                self.default_retrieval_mode = parsed;
            }
        }
        if let Some(s) = value.get("embedding_model").and_then(|v| v.as_str()) {
            self.embedding_model = s.to_string();
        }
        if let Some(a) = value.get("hybrid_alpha").and_then(|v| v.as_f64()) {
            self.hybrid_alpha = (a as f32).clamp(0.0, 1.0);
        }
        if let Some(k) = value.get("similar_chats_k").and_then(|v| v.as_u64()) {
            self.similar_chats_k = k as usize;
        }
        if let Some(n) = value.get("summary_refresh_chars").and_then(|v| v.as_u64()) {
            self.summary_refresh_chars = n as usize;
        }
        if let Some(n) = value.get("summary_delta_chars").and_then(|v| v.as_u64()) {
            self.summary_delta_chars = n as usize;
        }
        if let Some(n) = value.get("rag_top_k").and_then(|v| v.as_u64()) {
            self.rag_top_k = n as usize;
        }
        if let Some(b) = value.get("auto_tag").and_then(|v| v.as_bool()) {
            self.auto_tag = b;
        }
        // Embedding backend selection (string form)
        if let Some(backend_str) = value.get("embedding_backend").and_then(|v| v.as_str()) {
            self.embedding_backend = match backend_str {
                "fastembed" | "fast_embed" => {
                    EmbeddingBackendKind::FastEmbedLocal { model_path: None }
                }
                // These variants require keys; they will be filled in below if provided.
                "openai" => {
                    let model = self
                        .openai
                        .model
                        .clone()
                        .unwrap_or(self.embedding_model.clone());
                    EmbeddingBackendKind::OpenAI {
                        api_key: self.openai.api_key.clone().unwrap_or_default(),
                        model,
                    }
                }
                "azure_openai" | "azure-openai" | "azure" => {
                    let model = self
                        .azure_openai
                        .embedding_model
                        .clone()
                        .or(self.openai.model.clone())
                        .unwrap_or(self.embedding_model.clone());
                    EmbeddingBackendKind::AzureOpenAI {
                        api_key: self.azure_openai.api_key.clone().unwrap_or_default(),
                        endpoint: self.azure_openai.endpoint.clone().unwrap_or_default(),
                        model,
                    }
                }
                _ => self.embedding_backend.clone(),
            };
        }
        // OpenAI nested
        if let Some(openai) = value.get("openai") {
            if let Some(key) = openai.get("api_key").and_then(|v| v.as_str()) {
                self.openai.api_key = Some(key.to_string());
            }
            if let Some(url) = openai.get("api_url").and_then(|v| v.as_str()) {
                self.openai.api_url = Some(url.to_string());
            }
            if let Some(model) = openai.get("model").and_then(|v| v.as_str()) {
                self.openai.model = Some(model.to_string());
            }
        }
        // Azure nested
        if let Some(az) = value.get("azure_openai") {
            if let Some(key) = az.get("api_key").and_then(|v| v.as_str()) {
                self.azure_openai.api_key = Some(key.to_string());
            }
            if let Some(ep) = az.get("endpoint").and_then(|v| v.as_str()) {
                self.azure_openai.endpoint = Some(ep.to_string());
            }
            if let Some(ver) = az.get("api_version").and_then(|v| v.as_str()) {
                self.azure_openai.api_version = Some(ver.to_string());
            }
            if let Some(dep) = az.get("deployment").and_then(|v| v.as_str()) {
                self.azure_openai.deployment = Some(dep.to_string());
            }
            if let Some(model) = az.get("embedding_model").and_then(|v| v.as_str()) {
                self.azure_openai.embedding_model = Some(model.to_string());
            }
        }
        // After potential new keys, attempt to re-synchronize backend variants that require concrete values.
        self.reconcile_backend_variant();
    }

    /// Attempt to create a config from a root settings JSON value (the entire settings.json).
    pub fn from_settings_root(root: &serde_json::Value) -> Self {
        let mut cfg = Self::default();
        if let Some(ch) = root.get("chat_history") {
            cfg.apply_settings_object(ch);
        }
        cfg
    }

    /// Load settings from the conventional editor settings files (best effort).
    /// Looks for `.zed/settings.json` then `settings.json`.
    /// On any IO or parse error, returns defaults.
    pub fn load_from_default_files() -> Self {
        let candidates = [".zed/settings.json", "settings.json"];
        for path in candidates {
            if let Ok(text) = fs::read_to_string(path) {
                if let Ok(root) = serde_json::from_str::<serde_json::Value>(&text) {
                    return Self::from_settings_root(&root);
                }
            }
        }
        Self::default()
    }

    fn reconcile_backend_variant(&mut self) {
        match &mut self.embedding_backend {
            EmbeddingBackendKind::FastEmbedLocal { .. } => {}
            EmbeddingBackendKind::OpenAI { api_key, model } => {
                if let Some(k) = &self.openai.api_key {
                    *api_key = k.clone();
                }
                if let Some(m) = &self.openai.model {
                    *model = m.clone();
                }
            }
            EmbeddingBackendKind::AzureOpenAI {
                api_key,
                endpoint,
                model,
            } => {
                if let Some(k) = &self.azure_openai.api_key {
                    *api_key = k.clone();
                }
                if let Some(ep) = &self.azure_openai.endpoint {
                    *endpoint = ep.clone();
                }
                if let Some(m) = self
                    .azure_openai
                    .embedding_model
                    .clone()
                    .or(self.openai.model.clone())
                {
                    *model = m;
                }
            }
        }
    }
}

/// Abstraction over embedding backends.
#[async_trait]
pub trait EmbeddingBackend: Send + Sync {
    fn model_name(&self) -> &str;
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
}

/// Supported backend kinds (configuration surface).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum EmbeddingBackendKind {
    FastEmbedLocal {
        model_path: Option<String>,
    },
    OpenAI {
        api_key: String,
        model: String,
    },
    AzureOpenAI {
        api_key: String,
        endpoint: String,
        model: String,
    },
}
/// Retrieval strategy for search / RAG.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub enum RetrievalMode {
    Bm25,
    Embedding,
    Hybrid,
}

/// FastEmbed local backend stub.
pub struct FastEmbedBackend {
    model_name: String,
    cache: crate::fastembed_cache::FastEmbedCache,
}

impl FastEmbedBackend {
    pub fn new(model_name: impl Into<String>) -> Self {
        let name_string = model_name.into();
        let model_enum = crate::fastembed_cache::FastEmbedCache::resolve_model_name(&name_string);
        Self {
            model_name: name_string,
            cache: crate::fastembed_cache::FastEmbedCache::new(model_enum),
        }
    }
}

#[async_trait]
impl EmbeddingBackend for FastEmbedBackend {
    fn model_name(&self) -> &str {
        &self.model_name
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        // Delegate to shared cache (lazy init + mutex for interior mutability).
        self.cache.embed(texts)
    }
}

/// OpenAI backend stub.
pub struct OpenAIEmbeddingBackend {
    model_name: String,
    // TODO: add client handle.
}

impl OpenAIEmbeddingBackend {
    pub fn new(model_name: impl Into<String>) -> Self {
        Self {
            model_name: model_name.into(),
        }
    }
}

#[async_trait]
impl EmbeddingBackend for OpenAIEmbeddingBackend {
    fn model_name(&self) -> &str {
        &self.model_name
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Err(anyhow!("openai embedding backend not yet implemented"))
    }
}

/// Azure OpenAI backend stub.
pub struct AzureOpenAIEmbeddingBackend {
    model_name: String,
    endpoint: String,
    api_key: String,
    api_version: String,
    deployment: String,
}

impl AzureOpenAIEmbeddingBackend {
    pub fn new(model_name: impl Into<String>) -> Self {
        // Delegate to new_with_config with empty defaults so all struct fields are initialized.
        // Callers that need real Azure values should use new_with_config directly.
        Self::new_with_config(model_name, "", "", "", "")
    }

    /// Temporary constructor used by initialization code to pass Azure specifics.
    /// Stores all provided parameters for later REST embedding calls.
    pub fn new_with_config(
        model_name: impl Into<String>,
        endpoint: impl Into<String>,
        api_key: impl Into<String>,
        api_version: impl Into<String>,
        deployment: impl Into<String>,
    ) -> Self {
        Self {
            model_name: model_name.into(),
            endpoint: endpoint.into(),
            api_key: api_key.into(),
            api_version: api_version.into(),
            deployment: deployment.into(),
        }
    }
}

#[async_trait]
impl EmbeddingBackend for AzureOpenAIEmbeddingBackend {
    fn model_name(&self) -> &str {
        &self.model_name
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        // Azure OpenAI Embeddings REST:
        // POST {endpoint}/openai/deployments/{deployment}/embeddings?api-version={api_version}
        // Body: { "input": [...], "model": "<model or deployment>" }
        let url = format!(
            "{}/openai/deployments/{}/embeddings?api-version={}",
            self.endpoint.trim_end_matches('/'),
            self.deployment,
            self.api_version
        );

        #[derive(serde::Serialize)]
        struct AzureEmbeddingRequest<'a> {
            input: &'a [String],
            #[serde(skip_serializing_if = "Option::is_none")]
            model: Option<&'a str>,
        }

        let body_struct = AzureEmbeddingRequest {
            input: texts,
            model: if self.model_name == self.deployment {
                None
            } else {
                Some(self.model_name.as_str())
            },
        };

        let client = reqwest::Client::new();
        let resp = client
            .post(&url)
            .header("api-key", &self.api_key)
            .json(&body_struct)
            .send()
            .await
            .map_err(|e| anyhow!("azure embedding request failed: {e}"))?;

        let status = resp.status();
        let raw = resp.text().await.unwrap_or_default();

        #[derive(serde::Deserialize)]
        struct AzureEmbeddingData {
            embedding: Vec<f32>,
        }
        #[derive(serde::Deserialize)]
        struct AzureEmbeddingResponse {
            data: Vec<AzureEmbeddingData>,
        }

        if !status.is_success() {
            return Err(anyhow!(
                "azure embedding error status {} body: {}",
                status,
                raw
            ));
        }
        let parsed: AzureEmbeddingResponse = serde_json::from_str(&raw)
            .map_err(|e| anyhow!("failed to parse azure embedding response: {e}; body={raw}"))?;
        Ok(parsed.data.into_iter().map(|d| d.embedding).collect())
    }
}

/// Core store for chats and messages (persistence + retrieval + search).
pub struct ChatStore {
    backend: Arc<dyn EmbeddingBackend>,
    config: ChatHistoryConfig,
    /// Optional database-backed implementation for persistence / advanced search.
    db: Option<Arc<tokio::sync::Mutex<ChatHistoryDb>>>,
}

impl ChatStore {
    pub fn new(
        backend: Arc<dyn EmbeddingBackend>,
        config: ChatHistoryConfig,
        db: Option<Arc<tokio::sync::Mutex<ChatHistoryDb>>>,
    ) -> Self {
        Self {
            backend,
            config,
            db,
        }
    }

    /// Create a new chat.
    /// Persists the chat immediately if a database is attached.
    pub async fn create_chat(
        &self,
        project_id: Option<String>,
        title: Option<String>,
    ) -> Result<ChatMetadata> {
        let now = OffsetDateTime::now_utc();
        let meta = ChatMetadata {
            chat_id: ChatId(self.generate_chat_id()),
            project_id,
            title,
            summary: None,
            created_at: now,
            updated_at: now,
            total_messages: 0,
            total_characters: 0,
            summary_refreshed_characters: 0,
            token_estimate: 0,
            embedding_model: Some(self.backend.model_name().to_string()),
            archived: false,
            pinned: false,
            tags: Vec::new(),
            chat_vector: None,
        };
        if let Some(db_arc) = &self.db {
            let mut db = db_arc.lock().await;
            db.insert_chat(&meta).await?;
        }
        Ok(meta)
    }

    /// Append a message; intended to be called automatically by higher-level assistant code.
    /// Persists chat metadata, message row, tags, and updates BM25 index when a database is attached.
    pub async fn append_message(
        &self,
        chat: &mut ChatMetadata,
        role: MessageRole,
        content: String,
    ) -> Result<ChatMessage> {
        let message = ChatMessage {
            id: MessageId(self.generate_message_id()),
            chat_id: chat.chat_id.clone(),
            role,
            content: content.clone(),
            created_at: OffsetDateTime::now_utc(),
            token_estimate: 0,
            tags: Vec::new(),
            embedding_digest: None,
        };

        // Update chat metadata counters.
        chat.total_messages += 1;
        chat.total_characters += message.content.len();
        chat.updated_at = message.created_at;

        // Summary refresh & tag suggestion.
        if self.should_refresh_summary(chat) {
            self.refresh_summary(chat, &[message.clone()]);
        }
        if self.config.auto_tag {
            let new_tags = self.suggest_tags(&[message.clone()]);
            for t in new_tags {
                if !chat.tags.contains(&t) {
                    chat.tags.push(t);
                }
            }
        }

        if let Some(db_arc) = &self.db {
            let mut db = db_arc.lock().await;
            // Upsert chat row (insert if missing).
            match db.update_chat(chat).await {
                Ok(_) => {}
                Err(e) if e.to_string().contains("chat not found for update") => {
                    db.insert_chat(chat).await?;
                }
                Err(e) => return Err(e),
            }
            // Persist tags after any auto-tag additions.
            db.set_tags(&chat.chat_id, &chat.tags).await?;
            // Persist message and index for lexical search.
            db.insert_message(&message).await?;

            // Incremental chat vector update:
            // Every append we embed the new message (unless already cached) and
            // merge its vector into the existing pooled chat vector. Every 30th
            // message we perform a full recompute to avoid drift from incremental rounding.
            let new_vec_map = db.embed_messages_if_needed(&[message.clone()]).await?;
            if let Some(new_vec) = new_vec_map.get(&message.id) {
                let interval = self.config.chat_vector_recompute_interval.max(1);
                if chat.total_messages % interval == 0 {
                    // Periodic full recompute for numerical stability.
                    if let Some(pooled) = db.recompute_chat_embedding(&chat.chat_id).await? {
                        chat.chat_vector = Some(pooled);
                        // Persist updated chat metadata with new pooled vector.
                        db.update_chat(chat).await?;
                    }
                } else {
                    // Incremental mean pooling: new_avg = (old * (n-1) + new) / n
                    let n = chat.total_messages as f32;
                    if let Some(old_vec) = chat.chat_vector.take() {
                        if old_vec.len() == new_vec.len() && !old_vec.is_empty() {
                            let mut merged = old_vec;
                            for (i, v) in new_vec.iter().enumerate() {
                                merged[i] = (merged[i] * (n - 1.0) + *v) / n;
                            }
                            chat.chat_vector = Some(merged);
                        } else {
                            // Dimension mismatch or empty -> fallback to new vector only.
                            chat.chat_vector = Some(new_vec.clone());
                        }
                    } else {
                        // First vector.
                        chat.chat_vector = Some(new_vec.clone());
                    }
                    // Persist updated metadata (chat_vector changed).
                    db.update_chat(chat).await?;
                }
            }
        }

        Ok(message)
    }

    /// Decide whether summary should be refreshed based on configured thresholds.
    fn should_refresh_summary(&self, meta: &ChatMetadata) -> bool {
        if meta.total_characters < self.config.summary_refresh_chars {
            return false;
        }
        let delta = meta
            .total_characters
            .saturating_sub(meta.summary_refreshed_characters);
        delta >= self.config.summary_delta_chars
    }

    /// Refresh summary (placeholder: first 240 chars) and update tracking.
    fn refresh_summary(&self, meta: &mut ChatMetadata, messages: &[ChatMessage]) {
        let mut combined = String::new();
        for m in messages {
            if !combined.is_empty() {
                combined.push(' ');
            }
            combined.push_str(m.content.trim());
            if combined.len() >= 240 {
                break;
            }
        }
        let summary_text = if combined.len() > 240 {
            format!("{}…", &combined[..240])
        } else {
            combined
        };
        meta.summary = Some(summary_text);
        meta.summary_refreshed_characters = meta.total_characters;
        meta.updated_at = OffsetDateTime::now_utc();
    }

    /// Suggest tags based on simple token frequency; excludes short tokens and stopwords.
    fn suggest_tags(&self, messages: &[ChatMessage]) -> Vec<String> {
        use std::collections::HashMap;
        const STOPWORDS: &[&str] = &[
            "the", "and", "for", "with", "this", "that", "from", "into", "over", "when", "were",
            "have", "has", "had", "are", "was", "will", "shall", "would", "could", "should", "can",
            "a", "an", "of", "in", "on", "to", "at", "by", "it", "is", "as", "or", "be", "we",
            "you", "our", "your", "but", "not",
        ];
        let mut freq: HashMap<String, usize> = HashMap::new();
        for m in messages {
            for token in m.content.split(|c: char| !c.is_alphanumeric()) {
                if token.len() < 3 {
                    continue;
                }
                let t = token.to_ascii_lowercase();
                if STOPWORDS.contains(&t.as_str()) {
                    continue;
                }
                *freq.entry(t).or_insert(0) += 1;
            }
        }
        let mut items: Vec<(String, usize)> = freq.into_iter().collect();
        items.sort_by(|a, b| b.1.cmp(&a.1));
        items.into_iter().take(5).map(|(t, _)| t).collect()
    }

    /// Retrieve chat metadata + messages.
    pub async fn get_chat(&self, chat_id: &ChatId) -> Result<(ChatMetadata, Vec<ChatMessage>)> {
        if let Some(db_mutex) = &self.db {
            let db = db_mutex.lock().await;
            db.fetch_chat_with_messages(chat_id).await
        } else {
            Err(anyhow!("chat persistence unavailable"))
        }
    }

    /// List chats (project-scoped by default).
    pub async fn list_chats(
        &self,
        project_id: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<ChatMetadata>> {
        if let Some(db_mutex) = &self.db {
            let db = db_mutex.lock().await;
            db.list_chats(project_id, limit, offset).await
        } else {
            Err(anyhow!("chat persistence unavailable"))
        }
    }

    /// Find similar chats using chat-level embeddings.
    pub async fn similar_chats(
        &self,
        chat_id: &ChatId,
        n: usize,
        project_scoped: bool,
    ) -> Result<Vec<(ChatMetadata, f32)>> {
        if let Some(db_mutex) = &self.db {
            let db = db_mutex.lock().await;
            db.similar_chats(chat_id, n, project_scoped).await
        } else {
            Err(anyhow!("chat persistence unavailable"))
        }
    }

    /// Hybrid / bm25 / embedding search over messages.
    pub async fn search_messages(
        &self,
        query: &str,
        project_id: Option<&str>,
        chat_id: Option<&ChatId>,
        top_k: usize,
        mode: RetrievalMode,
        alpha: f32,
    ) -> Result<Vec<RetrievedContext>> {
        let db_mutex = self
            .db
            .as_ref()
            .ok_or_else(|| anyhow!("chat persistence unavailable"))?;
        let db = db_mutex.lock().await;
        let scored = db
            .search_messages(query, project_id, chat_id, top_k, mode, alpha)
            .await?;
        let contexts = scored
            .into_iter()
            .map(|(msg, score)| RetrievedContext {
                chat_id: msg.chat_id.clone(),
                message_id: msg.id.clone(),
                role: msg.role,
                content_excerpt: {
                    let c = msg.content.trim();
                    if c.len() > 200 {
                        format!("{}…", &c[..200])
                    } else {
                        c.to_string()
                    }
                },
                score,
            })
            .collect();
        Ok(contexts)
    }

    /// RAG question answering over retrieved prior messages.
    /// This placeholder implementation uses retrieved contexts directly to synthesize a lightweight answer.
    /// Future improvement: delegate to an LLM with a constructed prompt including citations.
    pub async fn answer_question(
        &self,
        question: &str,
        project_id: Option<&str>,
        chat_id: Option<&ChatId>,
        top_k: usize,
        mode: RetrievalMode,
        alpha: f32,
    ) -> Result<RagAnswer> {
        let contexts = self
            .search_messages(question, project_id, chat_id, top_k, mode, alpha)
            .await?;
        // Build a simple synthesized answer from top few contexts.
        let mut answer_parts = Vec::new();
        for c in contexts.iter().take(3) {
            let role = match c.role {
                MessageRole::User => "User",
                MessageRole::Assistant => "Assistant",
                MessageRole::System => "System",
                MessageRole::Tool => "Tool",
                MessageRole::Other(_) => "Other",
            };
            answer_parts.push(format!("{role}: {}", c.content_excerpt));
        }
        let answer = if answer_parts.is_empty() {
            "No relevant prior messages found.".to_string()
        } else {
            format!("Based on prior chats:\n{}", answer_parts.join("\n"))
        };
        Ok(RagAnswer { answer, contexts })
    }

    /// Recompute (or initially compute) pooled chat embeddings.
    /// If `chat_id` is provided, only that chat is processed; otherwise all known chats are iterated.
    /// Returns an error if persistence / embedding backend is unavailable.
    pub async fn reembed(&self, chat_id: Option<&ChatId>) -> Result<()> {
        let db_mutex = self
            .db
            .as_ref()
            .ok_or_else(|| anyhow!("chat persistence unavailable"))?;
        let mut db = db_mutex.lock().await;

        if let Some(id) = chat_id {
            let _ = db.recompute_chat_embedding(id).await?;
            return Ok(());
        }

        // Recompute for all chats (sequential to avoid monopolizing the foreground thread).
        let chats = db.list_chats(None, usize::MAX, 0).await?;
        for meta in chats {
            let _ = db.recompute_chat_embedding(&meta.chat_id).await?;
        }
        Ok(())
    }
    /// Update metadata fields (title, summary, tags, archived, pinned).
    /// Returns an error if persistence (db) is unavailable.
    pub async fn update_metadata(
        &self,
        chat_id: &ChatId,
        title: Option<String>,
        summary: Option<String>,
        tags_add: &[String],
        tags_remove: &[String],
        archived: Option<bool>,
        pinned: Option<bool>,
    ) -> Result<ChatMetadata> {
        let db_mutex = self
            .db
            .as_ref()
            .ok_or_else(|| anyhow!("chat persistence unavailable"))?;
        let mut db = db_mutex.lock().await;
        // Fetch current metadata (messages not needed here, ignore second tuple element)
        let (mut meta, _messages) = db.fetch_chat_with_messages(chat_id).await?;
        let mut changed = false;

        if let Some(t) = title {
            if meta.title.as_ref() != Some(&t) {
                meta.title = Some(t);
                changed = true;
            }
        }
        if let Some(s) = summary {
            if meta.summary.as_ref() != Some(&s) {
                meta.summary = Some(s);
                // Mark summary refresh point
                meta.summary_refreshed_characters = meta.total_characters;
                changed = true;
            }
        }

        // Tags: additions
        if !tags_add.is_empty() {
            for tag in tags_add {
                if !meta.tags.contains(tag) {
                    meta.tags.push(tag.clone());
                    changed = true;
                }
            }
        }
        // Tags: removals
        if !tags_remove.is_empty() {
            let before = meta.tags.len();
            meta.tags.retain(|t| !tags_remove.contains(t));
            if meta.tags.len() != before {
                changed = true;
            }
        }

        if let Some(a) = archived {
            if meta.archived != a {
                meta.archived = a;
                changed = true;
            }
        }
        if let Some(p) = pinned {
            if meta.pinned != p {
                meta.pinned = p;
                changed = true;
            }
        }

        if changed {
            meta.updated_at = OffsetDateTime::now_utc();
            db.update_chat(&meta).await?;
            db.set_tags(chat_id, &meta.tags).await?;
        }

        Ok(meta)
    }
    /// Current config snapshot.
    pub fn config(&self) -> &ChatHistoryConfig {
        &self.config
    }

    /// Replace config (e.g. user changed settings).
    pub fn set_config(&mut self, config: ChatHistoryConfig) {
        self.config = config;
    }

    fn generate_chat_id(&self) -> String {
        // TODO: Replace with UUID generator from existing workspace utility.
        format!("chat_{}", OffsetDateTime::now_utc().unix_timestamp_nanos())
    }

    fn generate_message_id(&self) -> String {
        format!("msg_{}", OffsetDateTime::now_utc().unix_timestamp_nanos())
    }

    /// Tool wrapper for answering a question using prior chat history.
    /// By default this omits the retrieved contexts from the returned result (privacy / brevity).
    /// To obtain contexts, callers should invoke a dedicated retrieval method first.
    pub async fn tool_answer(
        &self,
        question: String,
        project_id: Option<String>,
        chat_id: Option<String>,
        top_k: Option<usize>,
        mode: Option<RetrievalMode>,
        alpha: Option<f32>,
    ) -> Result<RagAnswer> {
        let mode = mode.unwrap_or(self.config.default_retrieval_mode.clone());
        let alpha = alpha.unwrap_or(self.config.hybrid_alpha);
        let chat_id_ref = chat_id.as_ref().map(|id| ChatId(id.clone()));
        let chat_id_ref = chat_id_ref.as_ref();
        let mut rag = self
            .answer_question(
                &question,
                project_id.as_deref(),
                chat_id_ref,
                top_k.unwrap_or(self.config.rag_top_k),
                mode,
                alpha,
            )
            .await?;
        // Strip contexts by default per requirement (retain answer only).
        rag.contexts.clear();
        Ok(rag)
    }

    /// Tool-facing append wrapper: creates chat if needed and appends message.
    pub async fn tool_append(
        &self,
        req: tools::ToolAppendRequest,
    ) -> Result<tools::ToolAppendResult> {
        let mut chat_meta = if let Some(chat_id) = req.chat_id {
            let (meta, _msgs) = self.get_chat(&ChatId(chat_id)).await?;
            meta
        } else {
            self.create_chat(req.project_id, req.title).await?
        };
        let message = self
            .append_message(&mut chat_meta, req.role, req.content)
            .await?;
        Ok(tools::ToolAppendResult {
            chat: chat_meta,
            message,
        })
    }

    /// Tool-facing search wrapper using current config defaults.
    pub async fn tool_search(
        &self,
        req: tools::ToolSearchRequest,
    ) -> Result<tools::ToolSearchResult> {
        let mode = req
            .mode
            .unwrap_or(self.config.default_retrieval_mode.clone());
        let alpha = req.alpha.unwrap_or(self.config.hybrid_alpha);
        let owned_chat_id = req.chat_id.as_ref().map(|id| ChatId(id.clone()));
        let chat_id_ref = owned_chat_id.as_ref();
        let contexts = self
            .search_messages(
                &req.query,
                req.project_id.as_deref(),
                chat_id_ref,
                req.top_k.unwrap_or(self.config.rag_top_k),
                mode,
                alpha,
            )
            .await?;
        Ok(tools::ToolSearchResult { contexts })
    }
}

/// Helper to compute pooled chat vector from message embeddings (mean pooling).
pub fn pool_chat_embedding(message_vectors: &[Vec<f32>]) -> Option<Vec<f32>> {
    if message_vectors.is_empty() {
        return None;
    }
    let dim = message_vectors[0].len();
    let mut acc = vec![0f32; dim];
    for v in message_vectors {
        if v.len() != dim {
            return None;
        }
        for (i, val) in v.iter().enumerate() {
            acc[i] += *val;
        }
    }
    let count = message_vectors.len() as f32;
    for a in &mut acc {
        *a /= count;
    }
    Some(acc)
}

/// Simple score fusion (alpha * cosine + (1-alpha) * normalized_keyword).
pub fn fuse_scores(cosine: f32, keyword_score: f32, alpha: f32) -> f32 {
    let a = alpha.clamp(0.0, 1.0);
    a * cosine + (1.0 - a) * keyword_score
}

/// Placeholder keyword scoring normalization.
pub fn normalize_keyword_score(raw: f32, max: f32) -> f32 {
    if max <= 0.0 {
        0.0
    } else {
        (raw / max).clamp(0.0, 1.0)
    }
}

/// In-memory cache placeholder (will evolve once persistence implemented).
pub struct InMemoryIndex {
    pub chat_vectors: HashMap<ChatId, Vec<f32>>,
}

impl InMemoryIndex {
    pub fn new() -> Self {
        Self {
            chat_vectors: HashMap::new(),
        }
    }
}
