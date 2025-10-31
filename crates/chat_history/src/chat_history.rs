use std::collections::{HashMap, HashSet};
use std::f32::EPSILON;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/* WHY: External callers (tools layer) expect stable opaque identifier types */
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct ChatId(String);

impl ChatId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct MessageId(String);

impl MessageId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalMode {
    Bm25,
    Embedding,
    Hybrid,
}

impl Default for RetrievalMode {
    fn default() -> Self {
        RetrievalMode::Hybrid
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OpenAIConfig {
    pub api_key: Option<String>,
    pub api_url: Option<String>,
    pub model: Option<String>,
    pub embedding_model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AzureOpenAIConfig {
    pub api_key: Option<String>,
    pub endpoint: Option<String>,
    pub api_version: Option<String>,
    pub deployment: Option<String>,
    pub embedding_model: Option<String>,
}

/* WHY: Fields reflect spec; adding fields is non-breaking, removing would break tools adapter. */
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ChatHistoryConfig {
    pub embedding_model: String,
    pub hybrid_alpha: f32,
    pub similar_chats_k: usize,
    pub summary_refresh_chars: usize,
    pub summary_delta_chars: usize,
    pub rag_top_k: usize,
    pub auto_tag: bool,
    pub default_retrieval_mode: RetrievalMode,
    pub openai: Option<OpenAIConfig>,
    pub azure_openai: Option<AzureOpenAIConfig>,
}

impl Default for ChatHistoryConfig {
    fn default() -> Self {
        Self {
            embedding_model: "bge-base-en-v1.5".into(),
            hybrid_alpha: 0.55,
            similar_chats_k: 8,
            summary_refresh_chars: 4_000,
            summary_delta_chars: 1_500,
            rag_top_k: 6,
            auto_tag: true,
            default_retrieval_mode: RetrievalMode::Hybrid,
            openai: None,
            azure_openai: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub message_id: MessageId,
    pub chat_id: ChatId,
    pub role: MessageRole,
    pub content: String,
    pub created_at: DateTime<Utc>,
    pub token_estimate: usize,
    pub digest: String,
    pub vector: Option<Vec<f32>>, // None until embedded
}

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
    pub chat_vector: Option<Vec<f32>>, // pooled mean of message vectors
    pub last_summary_at_chars: usize,  // chars at last summary generation
}

impl ChatMetadata {
    fn update_from_message(&mut self, msg: &ChatMessage) {
        self.total_messages += 1;
        self.total_characters += msg.content.len();
        self.token_estimate += msg.token_estimate;
        self.updated_at = msg.created_at;
    }
}

#[derive(Debug, Error)]
pub enum ChatHistoryError {
    #[error("chat not found: {0}")]
    ChatNotFound(String),
    #[error("invalid retrieval mode for current configuration")]
    InvalidRetrievalMode,
    #[error("embedding backend unavailable")]
    EmbeddingUnavailable,
    #[error("reembedding requires an embedding backend")]
    ReembedBackendMissing,
    #[error("internal invariant violated: {0}")]
    Invariant(String),
    #[error("configuration error: {0}")]
    Config(String),
}

pub type ChatResult<T> = std::result::Result<T, ChatHistoryError>;

/* WHY: Trait kept minimal to ease stubbing and replacement; async for potential remote calls. */
pub trait EmbeddingBackend: Send + Sync {
    fn model_name(&self) -> &str;
    fn embed(&self, batch: &[String]) -> Result<Vec<Vec<f32>>>;
}

/* WHY: FastEmbed stub placeholder; actual model loading omitted for reconstruction. */
#[cfg(feature = "embedding-fastembed")]
pub struct FastEmbedBackend {
    model: String,
    inner: Option<fastembed::TextEmbedding>,
}

#[cfg(feature = "embedding-fastembed")]
impl FastEmbedBackend {
    pub fn new(model: impl Into<String>) -> Self {
        use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
        let model_str = model.into();
        // Attempt to map a few common identifiers; fall back to default model.
        let chosen = match model_str.as_str() {
            // FastEmbed enum variants (best‑effort; if variant name changes this still compiles but may fallback)
            "all-MiniLM-L6-v2" | "all-miniLM-L6-v2" => Some(EmbeddingModel::AllMiniLml6V2),
            // Add more mappings here as needed.
            _ => None,
        };
        let inner = match chosen {
            Some(enum_model) => TextEmbedding::try_new(InitOptions {
                model_name: enum_model,
                ..Default::default()
            })
            .ok(),
            None => {
                // Try default init; if that fails we will degrade to heuristic embeddings.
                TextEmbedding::try_new(InitOptions::default()).ok()
            }
        };
        Self {
            model: model_str,
            inner,
        }
    }

    fn fallback_embed(&self, batch: &[String]) -> Vec<Vec<f32>> {
        // Deterministic character frequency fallback (same as previous stub) for graceful degradation.
        let mut outputs = Vec::with_capacity(batch.len());
        for text in batch {
            let mut counts = [0_f32; 26];
            let mut total = 0_f32;
            for b in text.bytes() {
                match b {
                    b'a'..=b'z' => {
                        counts[(b - b'a') as usize] += 1.0;
                        total += 1.0;
                    }
                    b'A'..=b'Z' => {
                        counts[(b - b'A') as usize] += 1.0;
                        total += 1.0;
                    }
                    _ => {}
                }
            }
            let mut v = counts.to_vec();
            if total > 0.0 {
                for c in &mut v {
                    *c /= total;
                }
            }
            outputs.push(v);
        }
        outputs
    }
}

#[cfg(feature = "embedding-fastembed")]
impl EmbeddingBackend for FastEmbedBackend {
    fn model_name(&self) -> &str {
        &self.model
    }
    fn embed(&self, batch: &[String]) -> Result<Vec<Vec<f32>>> {
        if let Some(inner) = &self.inner {
            // fastembed returns Result; propagate errors and fallback on failure.
            match inner.embed(batch.to_vec(), None) {
                Ok(v) => return Ok(v),
                Err(e) => {
                    // Log-worthy in a fuller implementation; degrade silently here.
                    let _ = e;
                }
            }
        }
        Ok(self.fallback_embed(batch))
    }
}

#[cfg(feature = "embedding-openai")]
pub struct OpenAIEmbeddingBackend {
    model: String,
    client: Option<async_openai::Client<async_openai::config::OpenAIConfig>>,
}

#[cfg(feature = "embedding-openai")]
impl OpenAIEmbeddingBackend {
    pub fn new(model: impl Into<String>) -> Self {
        // Rationale: rely on environment (OPENAI_API_KEY) if present; if not, degrade gracefully.
        let cfg = async_openai::config::OpenAIConfig::new();
        let client = if cfg.api_key().is_empty() {
            None
        } else {
            Some(async_openai::Client::with_config(cfg.clone()))
        };
        Self {
            model: model.into(),
            client,
        }
    }

    fn fallback_embed(&self, batch: &[String]) -> Vec<Vec<f32>> {
        // Simple deterministic length-based embedding (same dimensionality = 1) for degradation.
        batch
            .iter()
            .map(|t| vec![(t.len() as f32).sqrt()])
            .collect()
    }
}

#[cfg(feature = "embedding-openai")]
impl EmbeddingBackend for OpenAIEmbeddingBackend {
    fn model_name(&self) -> &str {
        &self.model
    }

    fn embed(&self, batch: &[String]) -> Result<Vec<Vec<f32>>> {
        // If no client (missing key), immediately fallback.
        let client = match &self.client {
            Some(c) => c,
            None => return Ok(self.fallback_embed(batch)),
        };

        // The async-openai API is async; block on current runtime if available.
        // We keep the sync trait contract by performing a blocking call.
        use async_openai::types::{CreateEmbeddingRequestArgs, EmbeddingInput};
        let request = CreateEmbeddingRequestArgs::default()
            .model(self.model.clone())
            .input(EmbeddingInput::StringArray(batch.clone()))
            .build()?;

        // Try to use an existing runtime; if absent, create a temporary one.
        let fut = async {
            client.embeddings().create(request).await.map(|resp| {
                resp.data
                    .into_iter()
                    .map(|d| d.embedding)
                    .collect::<Vec<Vec<f32>>>()
            })
        };

        let result = if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.block_on(fut)
        } else {
            // Create a minimal runtime just for this call.
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(fut)
        };

        match result {
            Ok(vectors) => {
                // Basic sanity: ensure we got one vector per input; otherwise fallback.
                if vectors.len() == batch.len() {
                    Ok(vectors)
                } else {
                    Ok(self.fallback_embed(batch))
                }
            }
            Err(_) => Ok(self.fallback_embed(batch)),
        }
    }
}

#[cfg(feature = "embedding-azure")]
pub struct AzureOpenAIEmbeddingBackend {
    model: String,
    endpoint: String,
    api_version: String,
    deployment: String,
    api_key: Option<String>,
}

#[cfg(feature = "embedding-azure")]
impl AzureOpenAIEmbeddingBackend {
    pub fn new(model: impl Into<String>) -> Self {
        // Environment-driven configuration (Azure typical env vars)
        let endpoint = std::env::var("AZURE_OPENAI_ENDPOINT").unwrap_or_default();
        let api_version =
            std::env::var("AZURE_OPENAI_API_VERSION").unwrap_or_else(|_| "2024-02-01".into());
        let deployment =
            std::env::var("AZURE_OPENAI_EMBEDDINGS_DEPLOYMENT").unwrap_or_else(|_| model.into());
        let api_key = std::env::var("AZURE_OPENAI_KEY").ok();
        Self {
            model: deployment.clone(),
            endpoint,
            api_version,
            deployment,
            api_key,
        }
    }
}

#[cfg(feature = "embedding-azure")]
impl EmbeddingBackend for AzureOpenAIEmbeddingBackend {
    fn model_name(&self) -> &str {
        &self.model
    }
    fn embed(&self, batch: &[String]) -> Result<Vec<Vec<f32>>> {
        // Fallback if required config missing
        if self.endpoint.is_empty()
            || self.deployment.is_empty()
            || self.api_key.as_ref().map(|k| k.is_empty()).unwrap_or(true)
        {
            return Ok(batch
                .iter()
                .map(|t| vec![(t.len() as f32).log2()])
                .collect());
        }
        // Build Azure embeddings URL:
        // {endpoint}/openai/deployments/{deployment}/embeddings?api-version={api_version}
        let url = format!(
            "{}/openai/deployments/{}/embeddings?api-version={}",
            self.endpoint.trim_end_matches('/'),
            self.deployment,
            self.api_version
        );
        // JSON body per Azure spec: { "input": [...], "model": "<ignored or deployment>" }
        #[derive(Serialize)]
        struct EmbeddingRequest<'a> {
            input: &'a [String],
        }
        #[derive(Deserialize)]
        struct EmbeddingResponse {
            data: Vec<EmbeddingDatum>,
        }
        #[derive(Deserialize)]
        struct EmbeddingDatum {
            embedding: Vec<f32>,
        }
        let client = reqwest::blocking::Client::new();
        let resp = client
            .post(&url)
            .header("api-key", self.api_key.clone().unwrap_or_default())
            .header("Content-Type", "application/json")
            .json(&EmbeddingRequest { input: batch })
            .send();
        let resp = match resp {
            Ok(r) => r,
            Err(_) => {
                return Ok(batch
                    .iter()
                    .map(|t| vec![(t.len() as f32).log2()])
                    .collect());
            }
        };
        if !resp.status().is_success() {
            return Ok(batch
                .iter()
                .map(|t| vec![(t.len() as f32).log2()])
                .collect());
        }
        let parsed: Result<EmbeddingResponse, _> = resp.json();
        match parsed {
            Ok(r) if r.data.len() == batch.len() => {
                Ok(r.data.into_iter().map(|d| d.embedding).collect())
            }
            _ => Ok(batch
                .iter()
                .map(|t| vec![(t.len() as f32).log2()])
                .collect()),
        }
    }
}

/* WHY: Lexical index kept intentionally simple (BM25‑ish scoring) to satisfy retrieval spec without external deps. */
#[derive(Default)]
struct LexicalIndex {
    // term -> chat_id -> message indices for quick candidate enumeration
    postings: HashMap<String, HashMap<ChatId, Vec<usize>>>,
    // message lengths (characters) for average length heuristic
    message_lengths: HashMap<MessageId, usize>,
    // quick access to (chat_id, message_index) -> message id for mapping
    // not storing reverse map for simplicity; we recompute on demand if needed
    total_messages: usize,
    total_length: usize,
}

impl LexicalIndex {
    fn index_message(&mut self, msg: &ChatMessage, msg_idx: usize) {
        let tokens = self.tokenize(&msg.content);
        let mut seen = HashSet::new();
        for tok in tokens {
            if !seen.insert(tok.clone()) {
                continue;
            }
            let entry = self.postings.entry(tok).or_default();
            entry.entry(msg.chat_id.clone()).or_default().push(msg_idx);
        }
        self.message_lengths
            .insert(msg.message_id.clone(), msg.content.len());
        self.total_messages += 1;
        self.total_length += msg.content.len();
    }

    fn avg_len(&self) -> f32 {
        if self.total_messages == 0 {
            0.0
        } else {
            self.total_length as f32 / self.total_messages as f32
        }
    }

    fn tokenize(&self, text: &str) -> Vec<String> {
        text.split(|c: char| !c.is_alphanumeric())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_lowercase())
            .collect()
    }

    fn bm25_scores(
        &self,
        query: &str,
        messages_in_chat: &[(usize, &ChatMessage)],
    ) -> HashMap<MessageId, f32> {
        if self.total_messages == 0 {
            return HashMap::new();
        }
        let k1 = 1.2_f32;
        let b = 0.75_f32;
        let avg_len = self.avg_len().max(1.0);
        let mut scores: HashMap<MessageId, f32> = HashMap::new();
        let q_tokens = self.tokenize(query);
        let mut unique_q = HashSet::new();
        for tok in q_tokens {
            if !unique_q.insert(tok.clone()) {
                continue;
            }
            let posting = match self.postings.get(&tok) {
                Some(p) => p,
                None => continue,
            };
            let mut doc_freq = 0_usize;
            for (_cid, idxs) in posting {
                doc_freq += idxs.len();
            }
            if doc_freq == 0 {
                continue;
            }
            let idf = ((self.total_messages - doc_freq + 0) as f32 + 0.5) / (doc_freq as f32 + 0.5);
            let idf = idf.max(EPSILON).ln(); // standard BM25 idf variant

            for (msg_idx, msg) in messages_in_chat {
                if let Some(chat_postings) = posting.get(&msg.chat_id) {
                    if !chat_postings.contains(msg_idx) {
                        continue;
                    }
                    // term frequency (binary presence is enough for simplicity)
                    let tf = 1.0_f32;
                    let len = self
                        .message_lengths
                        .get(&msg.message_id)
                        .copied()
                        .unwrap_or(msg.content.len()) as f32;
                    let norm = tf * (k1 + 1.0) / (tf + k1 * (1.0 - b + b * (len / avg_len)));
                    *scores.entry(msg.message_id.clone()).or_insert(0.0) += idf * norm;
                }
            }
        }
        scores
    }
}

pub struct ChatStore {
    config: ChatHistoryConfig,
    chats: HashMap<ChatId, ChatMetadata>,
    messages: HashMap<ChatId, Vec<ChatMessage>>,
    lexical: LexicalIndex,
    embedding_backend: Option<Arc<dyn EmbeddingBackend>>,
    pending_embedding: Vec<(ChatId, MessageId)>, // queue of messages needing embeddings
    batch_size: usize,
    embedding_worker_tx: Option<std::sync::mpsc::Sender<()>>, // signal channel for background embedding flush
}

impl ChatStore {
    /// Attempt to automatically initialize an embedding backend based on the
    /// current configuration's `embedding_model` if none is already present.
    /// This is a lightweight, idempotent best‑effort helper. It degrades
    /// silently if the requested backend cannot be constructed so callers
    /// can still rely on lexical retrieval.
    pub fn auto_init_backend(&mut self) {
        if self.embedding_backend.is_some() {
            return;
        }
        let model = self.config.embedding_model.trim().to_string();

        // Precedence rules (from highest to lowest):
        // 1. Explicit prefix "azure:" -> Azure backend (remainder optional deployment name)
        // 2. Explicit prefix "openai:" -> OpenAI backend (remainder optional model name)
        // 3. FastEmbed feature enabled -> local embedding
        // 4. OpenAI feature enabled (env/config present) -> OpenAI
        // 5. Azure feature enabled (env/config present) -> Azure
        // 6. Fallback: leave None (lexical only)
        //
        // Prefix parsing keeps syntax simple and avoids additional config surfaces.
        let parsed_prefix = model
            .split_once(':')
            .map(|(p, rest)| (p.to_lowercase(), rest.to_string()));

        match parsed_prefix {
            #[cfg(feature = "embedding-azure")]
            Some((p, rest)) if p == "azure" => {
                let backend_model = if rest.is_empty() { model.clone() } else { rest };
                let be = AzureOpenAIEmbeddingBackend::new(backend_model);
                self.embedding_backend = Some(Arc::new(be));
                return;
            }
            #[cfg(feature = "embedding-openai")]
            Some((p, rest)) if p == "openai" => {
                let backend_model = if rest.is_empty() { model.clone() } else { rest };
                let be = OpenAIEmbeddingBackend::new(backend_model);
                self.embedding_backend = Some(Arc::new(be));
                return;
            }
            _ => {}
        }

        // FastEmbed preferred when available and model does not explicitly request remote backends.
        #[cfg(feature = "embedding-fastembed")]
        {
            if parsed_prefix.is_none() {
                let be = FastEmbedBackend::new(model.clone());
                self.embedding_backend = Some(Arc::new(be));
                return;
            }
        }

        // If fastembed not chosen but explicit remote prefix absent, prefer OpenAI next.
        #[cfg(feature = "embedding-openai")]
        {
            if self.embedding_backend.is_none() && parsed_prefix.is_none() {
                let be = OpenAIEmbeddingBackend::new(model.clone());
                self.embedding_backend = Some(Arc::new(be));
                return;
            }
        }

        // Finally Azure if still none.
        #[cfg(feature = "embedding-azure")]
        {
            if self.embedding_backend.is_none() && parsed_prefix.is_none() {
                let be = AzureOpenAIEmbeddingBackend::new(model.clone());
                self.embedding_backend = Some(Arc::new(be));
                return;
            }
        }

        // Fallback: leave None (lexical retrieval only).
    }

    /// Start (idempotently) a very simple background worker loop that
    /// periodically attempts to flush any pending embeddings. Because the
    /// internal store is not wrapped in an Arc within this method, this
    /// function currently acts as a no‑op placeholder aside from recording
    /// that a worker was "started". A full implementation would move the
    /// store into an Arc<Mutex<..>> at a higher layer (e.g. SharedChatStore)
    /// and spawn a thread that locks, flushes, then sleeps.
    ///
    /// This placeholder still sets up a signaling channel so future
    /// refactors can attach an actual worker without changing the public
    /// method signature.
    pub fn start_embedding_worker(&mut self) {
        if self.embedding_worker_tx.is_some() {
            return;
        }
        // Background worker deferred: no Arc available within ChatStore itself.
        // We keep a signal channel for future external wiring (SharedChatStore).
        let (tx, _rx) = std::sync::mpsc::channel::<()>();
        self.embedding_worker_tx = Some(tx);
    }
    pub fn new(
        config: ChatHistoryConfig,
        embedding_backend: Option<Arc<dyn EmbeddingBackend>>,
    ) -> Self {
        let mut store = Self {
            config,
            chats: HashMap::new(),
            messages: HashMap::new(),
            lexical: LexicalIndex::default(),
            embedding_backend,
            pending_embedding: Vec::new(),
            batch_size: 32,
            embedding_worker_tx: None,
        };
        // Initialize backend automatically if none was explicitly supplied.
        store.auto_init_backend();
        // Start (placeholder) embedding worker channel.
        store.start_embedding_worker();
        store
    }

    pub fn config(&self) -> &ChatHistoryConfig {
        &self.config
    }

    pub fn set_config(&mut self, new_cfg: ChatHistoryConfig) {
        self.config = new_cfg;
    }

    pub fn update_config(&mut self, partial: ChatHistoryConfigPatch) -> &ChatHistoryConfig {
        if let Some(v) = partial.embedding_model {
            self.config.embedding_model = v;
        }
        if let Some(v) = partial.hybrid_alpha {
            self.config.hybrid_alpha = v;
        }
        if let Some(v) = partial.similar_chats_k {
            self.config.similar_chats_k = v;
        }
        if let Some(v) = partial.summary_refresh_chars {
            self.config.summary_refresh_chars = v;
        }
        if let Some(v) = partial.summary_delta_chars {
            self.config.summary_delta_chars = v;
        }
        if let Some(v) = partial.rag_top_k {
            self.config.rag_top_k = v;
        }
        if let Some(v) = partial.auto_tag {
            self.config.auto_tag = v;
        }
        if let Some(v) = partial.default_retrieval_mode {
            self.config.default_retrieval_mode = v;
        }
        if partial.openai.is_some() {
            self.config.openai = partial.openai;
        }
        if partial.azure_openai.is_some() {
            self.config.azure_openai = partial.azure_openai;
        }
        &self.config
    }

    pub fn append_message(
        &mut self,
        chat_id: Option<ChatId>,
        project_id: Option<String>,
        title: Option<String>,
        role: MessageRole,
        content: String,
    ) -> Result<(ChatMetadata, ChatMessage)> {
        let now = Utc::now();
        let cid = chat_id.unwrap_or_else(|| ChatId::new(Uuid::new_v4().to_string()));

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
                chat_vector: None,
                last_summary_at_chars: 0,
            };
            self.chats.insert(cid.clone(), meta);
            self.messages.insert(cid.clone(), Vec::new());
        }

        let token_estimate = Self::estimate_tokens(&content);
        let digest = Self::digest(&content);
        let msg = ChatMessage {
            message_id: MessageId::new(Uuid::new_v4().to_string()),
            chat_id: cid.clone(),
            role,
            content,
            created_at: now,
            token_estimate,
            digest,
            vector: None,
        };

        let list = self
            .messages
            .get_mut(&cid)
            .ok_or_else(|| anyhow!("messages container missing after init"))?;
        list.push(msg.clone());

        let msg_index = list.len() - 1;
        {
            let meta = self
                .chats
                .get_mut(&cid)
                .ok_or_else(|| anyhow!("metadata missing after init"))?;
            meta.update_from_message(&msg);
        }

        // Perform operations that require &mut self but not a simultaneous borrow of chat metadata.
        self.lexical.index_message(&msg, msg_index);
        self.pending_embedding
            .push((cid.clone(), msg.message_id.clone()));
        self.maybe_run_embedding_batch()?;

        // Potential summary refresh after other mutations while avoiding overlapping mutable borrows.
        // Avoid overlapping &mut borrows of self and chat metadata by delegating
        // summary logic to a method that acquires the metadata internally.
        self.maybe_trigger_summary_for(&cid);

        // Snapshot metadata for return.
        let meta_snapshot = self
            .chats
            .get(&cid)
            .ok_or_else(|| anyhow!("metadata missing after update"))?
            .clone();

        #[cfg(feature = "chat-persistence")]
        self.persist_after_change();
        Ok((meta_snapshot, msg))
    }

    fn maybe_run_embedding_batch(&mut self) -> Result<()> {
        if self.pending_embedding.is_empty() {
            return Ok(());
        }
        if self.pending_embedding.len() < self.batch_size {
            return Ok(()); // wait for more or eventual flush
        }
        self.flush_embeddings()
    }

    pub fn flush_embeddings(&mut self) -> Result<()> {
        if self.pending_embedding.is_empty() {
            return Ok(());
        }
        let backend = match &self.embedding_backend {
            Some(b) => b.clone(),
            None => {
                self.pending_embedding.clear();
                return Ok(()); // degrade silently; retrieval falls back to lexical
            }
        };
        let mut texts = Vec::with_capacity(self.pending_embedding.len());
        let mut msg_refs = Vec::with_capacity(self.pending_embedding.len());
        for (cid, mid) in &self.pending_embedding {
            if let Some(ms) = self.messages.get(cid) {
                if let Some(m) = ms.iter().find(|m| m.message_id == *mid) {
                    texts.push(m.content.clone());
                    msg_refs.push((cid.clone(), mid.clone()));
                }
            }
        }
        let rt_vecs = backend.embed(&texts)?;

        for ((cid, mid), vec) in msg_refs.into_iter().zip(rt_vecs.into_iter()) {
            if let Some(ms) = self.messages.get_mut(&cid) {
                if let Some(m) = ms.iter_mut().find(|m| m.message_id == mid) {
                    m.vector = Some(vec);
                }
            }
        }
        // Recompute pooled chat vectors (mean of message vectors) for chats touched.
        let mut touched = HashSet::new();
        for (cid, _) in self.pending_embedding.drain(..) {
            touched.insert(cid);
        }
        for cid in touched {
            self.recompute_chat_vector(&cid);
        }
        Ok(())
    }

    fn recompute_chat_vector(&mut self, chat_id: &ChatId) {
        let Some(chat_msgs) = self.messages.get(chat_id) else {
            return;
        };
        let mut acc: Option<Vec<f32>> = None;
        let mut count = 0_f32;
        for m in chat_msgs {
            if let Some(v) = &m.vector {
                match &mut acc {
                    Some(a) => {
                        if a.len() == v.len() {
                            for (ai, vi) in a.iter_mut().zip(v.iter()) {
                                *ai += *vi;
                            }
                            count += 1.0;
                        }
                    }
                    None => {
                        acc = Some(v.clone());
                        count = 1.0;
                    }
                }
            }
        }
        if let Some(vec) = acc.as_mut() {
            if count > 0.0 {
                for x in vec.iter_mut() {
                    *x /= count;
                }
            }
        }
        if let Some(meta) = self.chats.get_mut(chat_id) {
            meta.chat_vector = acc;
        }
    }

    pub fn list_chats(&self, offset: usize, limit: usize) -> Vec<ChatMetadata> {
        let mut v: Vec<ChatMetadata> = self.chats.values().cloned().collect();
        v.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        v.into_iter().skip(offset).take(limit).collect()
    }

    pub fn get_chat(&self, chat_id: &ChatId) -> ChatResult<(ChatMetadata, Vec<ChatMessage>)> {
        let meta = self
            .chats
            .get(chat_id)
            .ok_or_else(|| ChatHistoryError::ChatNotFound(chat_id.as_str().into()))?
            .clone();
        let msgs = self
            .messages
            .get(chat_id)
            .map(|v| v.clone())
            .unwrap_or_default();
        Ok((meta, msgs))
    }

    /// Update selected metadata fields for a chat.
    /// Each parameter is an Option indicating whether to mutate; for strings an inner Option sets/clears the value.
    /// Returns the updated metadata or an error if the chat does not exist.
    pub fn update_metadata(
        &mut self,
        chat_id: &ChatId,
        title: Option<Option<String>>,
        summary: Option<Option<String>>,
        archived: Option<bool>,
        pinned: Option<bool>,
        tags_add: Option<Vec<String>>,
        tags_remove: Option<Vec<String>>,
    ) -> ChatResult<ChatMetadata> {
        let meta = self
            .chats
            .get_mut(chat_id)
            .ok_or_else(|| ChatHistoryError::ChatNotFound(chat_id.as_str().into()))?;

        if let Some(t) = title {
            meta.title = t;
        }
        if let Some(s) = summary {
            meta.summary = s;
        }
        if let Some(a) = archived {
            meta.archived = a;
        }
        if let Some(p) = pinned {
            meta.pinned = p;
        }
        if let Some(add) = tags_add {
            for tag in add {
                if !meta.tags.iter().any(|existing| existing == &tag) {
                    meta.tags.push(tag);
                }
            }
        }
        if let Some(remove) = tags_remove {
            meta.tags.retain(|t| !remove.iter().any(|r| r == t));
        }
        meta.updated_at = Utc::now();
        let updated = meta.clone();
        // Drop mutable borrow before persistence call.
        #[cfg(feature = "chat-persistence")]
        self.persist_after_change();
        Ok(updated)
    }

    /* WHY: Legacy function signature retained for tools adapter compatibility (substring lexical search). */
    pub fn search_messages(
        &self,
        query: &str,
        chat_id: Option<&ChatId>,
        top_k: usize,
    ) -> Vec<ChatMessage> {
        let needle = query.to_lowercase();
        let mut out = Vec::new();
        let sources: Box<dyn Iterator<Item = (&ChatId, &Vec<ChatMessage>)>> =
            if let Some(cid) = chat_id {
                if let Some(m) = self.messages.get(cid) {
                    Box::new(std::iter::once((cid, m)))
                } else {
                    Box::new(std::iter::empty())
                }
            } else {
                Box::new(self.messages.iter())
            };
        for (_cid, msgs) in sources {
            for msg in msgs.iter().rev() {
                if msg.content.to_lowercase().contains(&needle) {
                    out.push(msg.clone());
                    if out.len() >= top_k {
                        return out;
                    }
                }
            }
        }
        out
    }

    pub fn hybrid_search(
        &self,
        query: &str,
        chat_id: Option<&ChatId>,
        top_k: usize,
        mode: Option<RetrievalMode>,
        alpha_override: Option<f32>,
    ) -> ChatResult<Vec<SearchHit>> {
        let mode = mode.unwrap_or(self.config.default_retrieval_mode);
        match mode {
            RetrievalMode::Bm25 => self.lexical_only(query, chat_id, top_k),
            RetrievalMode::Embedding => self.embedding_only(query, chat_id, top_k),
            RetrievalMode::Hybrid => {
                let lexical = self.lexical_only(query, chat_id, top_k)?;
                let embedding = self.embedding_only(query, chat_id, top_k)?;
                self.fuse_results(lexical, embedding, alpha_override)
            }
        }
    }

    fn lexical_only(
        &self,
        query: &str,
        chat_id: Option<&ChatId>,
        top_k: usize,
    ) -> ChatResult<Vec<SearchHit>> {
        // Gather candidate messages scope
        let sources: Box<dyn Iterator<Item = (&ChatId, &Vec<ChatMessage>)>> =
            if let Some(cid) = chat_id {
                if let Some(m) = self.messages.get(cid) {
                    Box::new(std::iter::once((cid, m)))
                } else {
                    Box::new(std::iter::empty())
                }
            } else {
                Box::new(self.messages.iter())
            };
        let mut pairs = Vec::new();
        for (cid, msgs) in sources {
            for (idx, msg) in msgs.iter().enumerate() {
                pairs.push((cid.clone(), idx, msg));
            }
        }
        let bm25_scores = self.lexical.bm25_scores(
            query,
            &pairs.iter().map(|(_, i, m)| (*i, *m)).collect::<Vec<_>>(),
        );
        let mut hits: Vec<SearchHit> = pairs
            .into_iter()
            .filter_map(|(_cid, _i, msg)| {
                bm25_scores
                    .get(&msg.message_id)
                    .copied()
                    .map(|s| SearchHit {
                        chat_id: msg.chat_id.clone(),
                        message_id: msg.message_id.clone(),
                        role: msg.role,
                        content: msg.content.clone(),
                        lexical_score: s,
                        embedding_score: 0.0,
                        fused_score: s,
                    })
            })
            .collect();
        hits.sort_by(|a, b| b.fused_score.total_cmp(&a.fused_score));
        hits.truncate(top_k);
        Ok(hits)
    }

    fn embedding_only(
        &self,
        query: &str,
        chat_id: Option<&ChatId>,
        top_k: usize,
    ) -> ChatResult<Vec<SearchHit>> {
        let Some(backend) = &self.embedding_backend else {
            return Ok(Vec::new()); // degrade
        };
        // Quick textual embedding (blocking) for query
        let query_vec = backend
            .embed(&[query.to_string()])
            .map_err(|_| ChatHistoryError::EmbeddingUnavailable)?
            .into_iter()
            .next()
            .unwrap_or_default();

        // Enumerate candidate messages
        let sources: Box<dyn Iterator<Item = (&ChatId, &Vec<ChatMessage>)>> =
            if let Some(cid) = chat_id {
                if let Some(m) = self.messages.get(cid) {
                    Box::new(std::iter::once((cid, m)))
                } else {
                    Box::new(std::iter::empty())
                }
            } else {
                Box::new(self.messages.iter())
            };

        let mut hits = Vec::new();
        for (_cid, msgs) in sources {
            for msg in msgs {
                if let Some(v) = &msg.vector {
                    let score = cosine(&query_vec, v);
                    hits.push(SearchHit {
                        chat_id: msg.chat_id.clone(),
                        message_id: msg.message_id.clone(),
                        role: msg.role,
                        content: msg.content.clone(),
                        lexical_score: 0.0,
                        embedding_score: score,
                        fused_score: score,
                    });
                }
            }
        }
        hits.sort_by(|a, b| b.fused_score.total_cmp(&a.fused_score));
        hits.truncate(top_k);
        Ok(hits)
    }

    fn fuse_results(
        &self,
        mut lexical: Vec<SearchHit>,
        mut embedding: Vec<SearchHit>,
        alpha_override: Option<f32>,
    ) -> ChatResult<Vec<SearchHit>> {
        let alpha = alpha_override.unwrap_or(self.config.hybrid_alpha);
        let mut by_key: HashMap<(ChatId, MessageId), SearchHit> = HashMap::new();
        for h in lexical.drain(..) {
            by_key.insert(
                (h.chat_id.clone(), h.message_id.clone()),
                SearchHit {
                    fused_score: h.lexical_score,
                    ..h
                },
            );
        }
        for h in embedding.drain(..) {
            by_key
                .entry((h.chat_id.clone(), h.message_id.clone()))
                .and_modify(|existing| {
                    existing.embedding_score = h.embedding_score;
                })
                .or_insert(h);
        }
        // Compute final fusion
        for h in by_key.values_mut() {
            let emb = h.embedding_score;
            let lex = h.lexical_score;
            h.fused_score = alpha * emb + (1.0 - alpha) * lex;
        }
        let mut all: Vec<SearchHit> = by_key.into_values().collect();
        all.sort_by(|a, b| b.fused_score.total_cmp(&a.fused_score));
        all.truncate(self.config.rag_top_k.max(1));
        Ok(all)
    }

    pub fn similar_chats(&self, target: &ChatId, k: Option<usize>) -> ChatResult<Vec<SimilarChat>> {
        let tgt_meta = self
            .chats
            .get(target)
            .ok_or_else(|| ChatHistoryError::ChatNotFound(target.as_str().into()))?;
        let Some(tgt_vec) = &tgt_meta.chat_vector else {
            return Ok(Vec::new());
        };
        let mut sims = Vec::new();
        for (cid, meta) in &self.chats {
            if cid == target {
                continue;
            }
            if let Some(v) = &meta.chat_vector {
                let score = cosine(tgt_vec, v);
                sims.push(SimilarChat {
                    chat: meta.clone(),
                    score,
                });
            }
        }
        sims.sort_by(|a, b| b.score.total_cmp(&a.score));
        let limit = k.unwrap_or(self.config.similar_chats_k);
        sims.truncate(limit);
        Ok(sims)
    }

    pub fn reembed(&mut self, chat_id: Option<&ChatId>, force: bool) -> ChatResult<()> {
        let Some(backend) = &self.embedding_backend else {
            return Err(ChatHistoryError::ReembedBackendMissing);
        };
        let target_chats: Vec<ChatId> = if let Some(cid) = chat_id {
            vec![cid.clone()]
        } else {
            self.chats.keys().cloned().collect()
        };
        let mut all_texts = Vec::new();
        let mut mapping = Vec::new();
        for cid in target_chats {
            if let Some(msgs) = self.messages.get_mut(&cid) {
                for m in msgs.iter_mut() {
                    if force {
                        m.vector = None;
                    }
                    if m.vector.is_none() {
                        all_texts.push(m.content.clone());
                        mapping.push((cid.clone(), m.message_id.clone()));
                    }
                }
            }
        }
        if all_texts.is_empty() {
            return Ok(());
        }
        let vecs = backend
            .embed(&all_texts)
            .map_err(|_| ChatHistoryError::EmbeddingUnavailable)?;
        for ((cid, mid), v) in mapping.into_iter().zip(vecs.into_iter()) {
            if let Some(msgs) = self.messages.get_mut(&cid) {
                if let Some(m) = msgs.iter_mut().find(|m| m.message_id == mid) {
                    m.vector = Some(v);
                }
            }
        }
        // Recompute chat vectors
        let distinct: HashSet<ChatId> = self.chats.keys().cloned().collect();
        for cid in distinct {
            self.recompute_chat_vector(&cid);
        }
        Ok(())
    }

    fn maybe_trigger_summary_for(&mut self, chat_id: &ChatId) {
        let Some(meta) = self.chats.get_mut(chat_id) else {
            return;
        };
        if meta.total_characters < self.config.summary_refresh_chars {
            return;
        }
        let delta = meta.total_characters - meta.last_summary_at_chars;
        if delta < self.config.summary_delta_chars {
            return;
        }
        if let Some(msgs) = self.messages.get(chat_id) {
            let mut acc = String::new();
            for m in msgs.iter().take(8) {
                if acc.len() + m.content.len() > 512 {
                    break;
                }
                acc.push_str(&m.content);
                acc.push(' ');
            }
            if acc.len() > 500 {
                acc.truncate(500);
                acc.push_str("…");
            }
            meta.summary = Some(acc.trim().to_string());
            meta.last_summary_at_chars = meta.total_characters;
            meta.updated_at = Utc::now();
        }
    }

    fn digest(content: &str) -> String {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in content.as_bytes().iter().take(256) {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        format!("{:016x}", h)
    }

    fn estimate_tokens(text: &str) -> usize {
        (text.len() / 4).max(1)
    }

    #[cfg(feature = "chat-persistence")]
    fn persist_after_change(&self) {
        // log::error imported via crate dependency; explicit local use removed
        // Lightweight full-store snapshot serialization (O(n)) invoked after mutating operations.
        #[derive(serde::Serialize)]
        struct ChatRecord<'a> {
            metadata: &'a ChatMetadata,
            messages: &'a [ChatMessage],
        }
        #[derive(serde::Serialize)]
        struct Snapshot<'a> {
            version: u32,
            chats: Vec<ChatRecord<'a>>,
        }
        let mut chats = Vec::with_capacity(self.chats.len());
        for meta in self.chats.values() {
            if let Some(msgs) = self.messages.get(&meta.chat_id) {
                chats.push(ChatRecord {
                    metadata: meta,
                    messages: msgs,
                });
            } else {
                chats.push(ChatRecord {
                    metadata: meta,
                    messages: &[],
                });
            }
        }
        let snap = Snapshot { version: 1, chats };
        if let Ok(json) = serde_json::to_string(&snap) {
            if let Err(e) = db::smol::block_on(
                db::kvp::KEY_VALUE_STORE.write_kvp("chat_history_snapshot_v1".to_string(), json),
            ) {
                error!("chat_history: snapshot write failed: {e:?}");
            }
        }
    }
}

/* WHY: Partial config patch used by adapter to apply runtime mutations without constructing a full config. */
#[derive(Debug, Default)]
pub struct ChatHistoryConfigPatch {
    pub embedding_model: Option<String>,
    pub hybrid_alpha: Option<f32>,
    pub similar_chats_k: Option<usize>,
    pub summary_refresh_chars: Option<usize>,
    pub summary_delta_chars: Option<usize>,
    pub rag_top_k: Option<usize>,
    pub auto_tag: Option<bool>,
    pub default_retrieval_mode: Option<RetrievalMode>,
    pub openai: Option<OpenAIConfig>,
    pub azure_openai: Option<AzureOpenAIConfig>,
}

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub chat_id: ChatId,
    pub message_id: MessageId,
    pub role: MessageRole,
    pub content: String,
    pub lexical_score: f32,
    pub embedding_score: f32,
    pub fused_score: f32,
}

#[derive(Debug, Clone)]
pub struct SimilarChat {
    pub chat: ChatMetadata,
    pub score: f32,
}

/* WHY: Helper trait for consistent ordering without allocating intermediate Vec for keys. */

/* WHY: Cosine used in multiple retrieval pathways; tolerant of dimension mismatch (returns 0). */
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0_f32;
    let mut na = 0.0_f32;
    let mut nb = 0.0_f32;
    for (ai, bi) in a.iter().zip(b.iter()) {
        dot += ai * bi;
        na += ai * ai;
        nb += bi * bi;
    }
    if na <= 0.0 || nb <= 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/* WHY: Thread-safe shared handle pattern already used by tools adapter. */
#[derive(Clone)]
pub struct SharedChatStore(Arc<Mutex<ChatStore>>);

#[cfg(feature = "chat-persistence")]
pub mod chat_history_db;

impl SharedChatStore {
    pub fn new(store: ChatStore) -> Self {
        Self(Arc::new(Mutex::new(store)))
    }
    pub fn lock(&self) -> parking_lot::MutexGuard<'_, ChatStore> {
        self.0.lock()
    }

    /// (Persistence scaffold) Save all chats + messages using provided callback.
    /// No-op unless the `chat-persistence` feature is enabled.
    #[cfg(feature = "chat-persistence")]
    pub fn persist_all<F>(&self, mut save: F) -> anyhow::Result<()>
    where
        F: FnMut(&ChatMetadata, &ChatMessage) -> anyhow::Result<()>,
    {
        let guard = self.0.lock();
        for meta in guard.chats.values() {
            // Emit a synthetic zero-length message save to guarantee metadata row exists first if desired.
            save(
                meta,
                &ChatMessage {
                    message_id: MessageId::new("META"),
                    chat_id: meta.chat_id.clone(),
                    role: MessageRole::User,
                    content: String::new(),
                    created_at: meta.updated_at,
                    token_estimate: 0,
                    digest: String::new(),
                    vector: None,
                },
            )?;
            if let Some(msgs) = guard.messages.get(&meta.chat_id) {
                for m in msgs {
                    save(meta, m)?;
                }
            }
        }
        Ok(())
    }

    /// (Persistence scaffold) Load chats/messages from an iterator of (metadata, messages).
    /// Existing in-memory data is replaced.
    #[cfg(feature = "chat-persistence")]
    pub fn load_from<I, M>(&self, iter: I)
    where
        I: IntoIterator<Item = (ChatMetadata, Vec<ChatMessage>)>,
    {
        let mut guard = self.0.lock();
        guard.chats.clear();
        guard.messages.clear();
        for (meta, msgs) in iter {
            let cid = meta.chat_id.clone();
            guard.chats.insert(cid.clone(), meta);
            guard.messages.insert(cid.clone(), msgs);
        }
    }
}

/* ---- Tests (focused on core invariants) ---- */

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> ChatStore {
        ChatStore::new(ChatHistoryConfig::default(), None)
    }

    #[test]
    fn append_and_get_roundtrip() {
        let mut s = store();
        let (meta, msg) = s
            .append_message(
                None,
                Some("proj".into()),
                Some("Title".into()),
                MessageRole::User,
                "Hello retrieval world".into(),
            )
            .expect("append");
        assert_eq!(meta.total_messages, 1);
        assert_eq!(msg.content, "Hello retrieval world");
        let (meta2, msgs) = s.get_chat(&meta.chat_id).expect("get");
        assert_eq!(meta2.chat_id, meta.chat_id);
        assert_eq!(msgs.len(), 1);
    }

    #[test]
    fn substring_search_legacy() {
        let mut s = store();
        let cid = ChatId::new("c1");
        s.append_message(
            Some(cid.clone()),
            None,
            None,
            MessageRole::User,
            "Rust design token".into(),
        )
        .unwrap();
        s.append_message(
            Some(cid.clone()),
            None,
            None,
            MessageRole::Assistant,
            "Another line".into(),
        )
        .unwrap();
        let r = s.search_messages("token", Some(&cid), 5);
        assert_eq!(r.len(), 1);
        assert!(r[0].content.contains("token"));
    }

    #[test]
    fn hybrid_fallback_without_embeddings() {
        let mut s = store();
        s.append_message(
            None,
            None,
            None,
            MessageRole::User,
            "Alpha beta gamma".into(),
        )
        .unwrap();
        s.append_message(
            None,
            None,
            None,
            MessageRole::Assistant,
            "Gamma delta epsilon".into(),
        )
        .unwrap();
        let hits = s
            .hybrid_search("gamma", None, 5, Some(RetrievalMode::Hybrid), None)
            .expect("search");
        assert!(!hits.is_empty());
    }

    #[test]
    fn config_patch() {
        let mut s = store();
        s.update_config(ChatHistoryConfigPatch {
            hybrid_alpha: Some(0.9),
            rag_top_k: Some(12),
            ..Default::default()
        });
        assert!((s.config.hybrid_alpha - 0.9).abs() < 1e-6);
        assert_eq!(s.config.rag_top_k, 12);
    }
}
