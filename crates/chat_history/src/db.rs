use anyhow::{anyhow, Context, Result};
// NOTE: Pending edit: need precise line numbers for fetch_message_vectors implementation block to insert
// actual embedding retrieval join against global embeddings table. Please provide a numbered excerpt
// (e.g. 20 lines before and after fetch_message_vectors) so I can add:
//  - Embeddings entity mirror (if not already available in scope)
//  - Query joining message_embeddings -> embeddings to populate real vectors
//  - Replacement of placeholder empty vectors with real dimension data.

use sea_orm::{
    ActiveModelTrait, IntoActiveModel, ConnectionTrait, QuerySelect, QueryTrait, EntityOrSelect, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    Set, TransactionTrait,
};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use std::collections::HashMap;

use crate::{
    pool_chat_embedding, ChatHistoryConfig, ChatId, ChatMetadata, ChatMessage, EmbeddingBackend,
    EmbeddingBackendKind, MessageId, MessageRole, RetrievalMode,
};
use crate::entities::{
    active_chat_from_metadata, active_message_from_chat_message, chat_metadata_from_model,
    chat_message_from_model, resolve_embedding_model_name, ChatMessageModelEntity,
    ChatModelEntity, ChatTagModelEntity, MessageEmbeddingModelEntity,
    // Refactored: entities now organized into submodules (chats, chat_messages, chat_tags, message_embeddings).
};

/// Digest of normalized content (UTF-8 lowercased).
pub fn compute_digest(text: &str) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(text.to_lowercase().as_bytes());
    hasher.finalize().to_vec()
}

/// Lightweight lexical tokenization (alphanumeric sequences, lowercased).
fn tokenize(content: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for c in content.chars() {
        if c.is_alphanumeric() {
            current.push(c.to_ascii_lowercase());
        } else if !current.is_empty() {
            tokens.push(current.clone());
            current.clear();
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Simple BM25 index skeleton. Scores are placeholders until full integration.
pub struct Bm25Index {
    /// Inverted: token -> (message_id, freq)
    inverted: std::collections::HashMap<String, Vec<(MessageId, u32)>>,
    /// Message lengths (token count).
    lengths: std::collections::HashMap<MessageId, usize>,
    total_docs: usize,
    avg_doc_len: f32,
}

impl Bm25Index {
    pub fn new() -> Self {
        Self {
            inverted: Default::default(),
            lengths: Default::default(),
            total_docs: 0,
            avg_doc_len: 0.0,
        }
    }

    pub fn add_message(&mut self, message: &ChatMessage) {
        let tokens = tokenize(&message.content);
        if tokens.is_empty() {
            return;
        }
        self.total_docs += 1;
        self.lengths.insert(message.id.clone(), tokens.len());
        let mut freq_map = std::collections::HashMap::<String, u32>::new();
        for t in tokens {
            *freq_map.entry(t).or_insert(0) += 1;
        }
        for (token, freq) in freq_map {
            self.inverted
                .entry(token)
                .or_default()
                .push((message.id.clone(), freq));
        }
        self.avg_doc_len =
            (self.lengths.values().sum::<usize>() as f32) / (self.lengths.len() as f32);
    }

    /// Very rough BM25-like score; k1,b constants hardcoded.
    pub fn search(&self, query: &str, limit: usize) -> Vec<(MessageId, f32)> {
        let k1 = 1.2;
        let b = 0.75;
        let mut scores = std::collections::HashMap::<MessageId, f32>::new();
        let q_tokens = tokenize(query);
        if q_tokens.is_empty() {
            return Vec::new();
        }
        for token in q_tokens {
            if let Some(postings) = self.inverted.get(&token) {
                let df = postings.len() as f32;
                if df == 0.0 || self.total_docs == 0 {
                    continue;
                }
                let n = self.total_docs as f32;
                let idf = (((n - df + 0.5) / (df + 0.5)) + 1.0).ln();
                for (msg_id, freq) in postings {
                    let freq = *freq;
                    let doc_len = *self.lengths.get(msg_id).unwrap_or(&1) as f32;
                    let norm = freq as f32 * (k1 + 1.0)
                        / (freq as f32
                            + k1 * (1.0 - b + b * (doc_len / self.avg_doc_len.max(1.0))));
                    *scores.entry(msg_id.clone()).or_insert(0.0) += idf * norm;
                }
            }
        }
        let mut scored: Vec<_> = scores.into_iter().collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        scored
    }
}

/// Database + indexing facade. Embedding computation is delegated out (async).
pub struct ChatHistoryDb {
    conn: DatabaseConnection,
    config: ChatHistoryConfig,
    embedding_backend: std::sync::Arc<dyn EmbeddingBackend>,
    bm25_index: Bm25Index,
    embedding_store: EmbeddingStore,
}

impl ChatHistoryDb {
    pub fn new(
        conn: DatabaseConnection,
        config: ChatHistoryConfig,
        embedding_backend: std::sync::Arc<dyn EmbeddingBackend>,
    ) -> Self {
        let embedding_store = EmbeddingStore::new(config.embedding_model.clone());
        Self {
            conn,
            config,
            embedding_backend,
            bm25_index: Bm25Index::new(),
            embedding_store,
        }
    }

    /// Insert new chat metadata + tags (tags initially empty).
    pub async fn insert_chat(&self, meta: &ChatMetadata) -> Result<()> {
        let active = active_chat_from_metadata(meta);
        active.insert(&self.conn).await?;
        Ok(())
    }

    /// Update an existing chat metadata (assumes exists).
    pub async fn update_chat(&self, meta: &ChatMetadata) -> Result<()> {
        let active = ChatModelEntity::find()
            .filter(crate::entities::ChatColumn::Id.eq(meta.chat_id.0.clone()))
            .one(&self.conn)
            .await?
            .context("chat not found for update")?;

        let mut active_model = active.into_active_model();
        crate::entities::apply_metadata_update(&mut active_model, meta);
        active_model.update(&self.conn).await?;
        Ok(())
    }

    /// Add or replace tags for a chat (set semantics).
    pub async fn set_tags(&self, chat_id: &ChatId, tags: &[String]) -> Result<()> {
        // Transaction to avoid partial modifications.
        let txn = self.conn.begin().await?;
        // Delete existing
        ChatTagModelEntity::delete_many()
            .filter(crate::entities::ChatTagColumn::ChatId.eq(chat_id.0.clone()))
            .exec(&txn)
            .await?;
        // Insert new
        for tag in tags {
            let active = crate::entities::ChatTagActiveModel {
                chat_id: Set(chat_id.0.clone()),
                tag: Set(tag.clone()),
            };
            active.insert(&txn).await?;
        }
        txn.commit().await?;
        Ok(())
    }

    pub async fn fetch_tags(&self, chat_id: &ChatId) -> Result<Vec<String>> {
        let rows = ChatTagModelEntity::find()
            .filter(crate::entities::ChatTagColumn::ChatId.eq(chat_id.0.clone()))
            .all(&self.conn)
            .await?;
        Ok(rows.into_iter().map(|r| r.tag).collect())
    }

    /// Insert a message and update in-memory BM25 index (embedding scheduling handled externally).
    pub async fn insert_message(&mut self, message: &ChatMessage) -> Result<()> {
        let active = active_message_from_chat_message(message);
        active.insert(&self.conn).await?;
        self.bm25_index.add_message(message);
        Ok(())
    }

    /// Associate a message with an embedding digest + model.
    pub async fn insert_message_embedding(
        &self,
        message_id: &MessageId,
        model: &str,
        digest: &[u8],
    ) -> Result<()> {
        let active = crate::entities::MessageEmbeddingActiveModel {
            message_id: Set(message_id.0.clone()),
            model: Set(model.to_string()),
            digest: Set(digest.to_vec()),
        };
        active.insert(&self.conn).await?;
        Ok(())
    }

    pub async fn get_message_embedding_digest(
        &self,
        message_id: &MessageId,
    ) -> Result<Option<Vec<u8>>> {
        let row = MessageEmbeddingModelEntity::find()
            .filter(crate::entities::MessageEmbeddingColumn::MessageId.eq(message_id.0.clone()))
            .one(&self.conn)
            .await?;
        Ok(row.map(|r| r.digest))
    }

    /// Delete a chat and all associated data (messages, tags, embeddings).
    /// Uses CASCADE foreign keys to automatically remove related records.
    pub async fn delete_chat(&self, chat_id: &ChatId) -> Result<()> {
        // Delete the chat; CASCADE handles messages, tags, and message_embeddings
        let result = ChatModelEntity::delete_many()
            .filter(crate::entities::ChatColumn::Id.eq(chat_id.0.clone()))
            .exec(&self.conn)
            .await?;

        if result.rows_affected == 0 {
            return Err(anyhow!("chat not found: {}", chat_id.0));
        }

        Ok(())
    }

    /// Fetch chat metadata and all messages ordered by created_at ascending.
    pub async fn fetch_chat_with_messages(
        &self,
        chat_id: &ChatId,
    ) -> Result<(ChatMetadata, Vec<ChatMessage>)> {
        let chat_model = ChatModelEntity::find()
            .filter(crate::entities::ChatColumn::Id.eq(chat_id.0.clone()))
            .one(&self.conn)
            .await?
            .context("chat not found")?;
        let tags = self.fetch_tags(chat_id).await?;
        let meta = chat_metadata_from_model(&chat_model, &tags)?;
        let msgs = ChatMessageModelEntity::find()
            .filter(crate::entities::ChatMessageColumn::ChatId.eq(chat_id.0.clone()))
            .order_by_asc(crate::entities::ChatMessageColumn::CreatedAt)
            .all(&self.conn)
            .await?;
        let messages = msgs.iter().map(chat_message_from_model).collect();
        Ok((meta, messages))
    }

    /// List chats (project scoped if provided) with paging.
    pub async fn list_chats(
        &self,
        project_id: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<ChatMetadata>> {
        let mut query = ChatModelEntity::find().order_by_desc(crate::entities::ChatColumn::UpdatedAt);
        if let Some(pid) = project_id {
            query = query.filter(crate::entities::ChatColumn::ProjectId.eq(pid.to_string()));
        }
        let rows = query
            .offset(offset as u64)
            .limit(limit as u64)
            .all(&self.conn)
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let tags = self.fetch_tags(&ChatId(row.id.clone())).await?;
            out.push(chat_metadata_from_model(&row, &tags)?);
        }
        Ok(out)
    }

    /// Rebuild BM25 index from all messages (used after startup or config changes).
    pub async fn rebuild_message_index(&mut self) -> Result<()> {
        self.bm25_index = Bm25Index::new();
        // Stream all messages; for scale considerations pagination could be added later.
        let rows = ChatMessageModelEntity::find().all(&self.conn).await?;
        for row in rows {
            let msg = chat_message_from_model(&row);
            self.bm25_index.add_message(&msg);
        }
        Ok(())
    }

    /// Compute pooled chat embedding given message vectors (embedding retrieval executed externally).
    pub fn compute_chat_embedding(&self, vectors: &[Vec<f32>]) -> Option<Vec<f32>> {
        pool_chat_embedding(vectors)
    }

    /// Similar chats using chat_vector cosine similarity.
    pub async fn similar_chats(
        &self,
        chat_id: &ChatId,
        n: usize,
        project_scoped: bool,
    ) -> Result<Vec<(ChatMetadata, f32)>> {
        let (target_meta, _) = self.fetch_chat_with_messages(chat_id).await?;
        let target_vec = if let Some(v) = &target_meta.chat_vector {
            v
        } else {
            return Ok(Vec::new());
        };

        let candidate_rows = if project_scoped {
            ChatModelEntity::find()
                .filter(crate::entities::ChatColumn::ProjectId.eq(
                    target_meta.project_id.clone().unwrap_or_default(),
                ))
                .all(&self.conn)
                .await?
        } else {
            ChatModelEntity::find().all(&self.conn).await?
        };

        let mut scored = Vec::new();
        for row in candidate_rows {
            if row.id == chat_id.0 {
                continue;
            }
            if let Some(bytes) = &row.chat_vector {
                let other_vec = crate::entities::decode_chat_vector(bytes)?;
                let cosine = cosine_similarity(target_vec, &other_vec);
                if cosine.is_finite() {
                    let tags = self.fetch_tags(&ChatId(row.id.clone())).await?;
                    let meta = chat_metadata_from_model(&row, &tags)?;
                    scored.push((meta, cosine));
                }
            }
        }
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(n);
        Ok(scored)
    }

    /// Message search with retrieval mode selection.
	    pub async fn search_messages(
	        &self,
	        query: &str,
	        project_id: Option<&str>,
	        chat_id: Option<&ChatId>,
	        top_k: usize,
	        mode: RetrievalMode,
	        alpha: f32,
	    ) -> Result<Vec<(ChatMessage, f32)>> {
	        let mut candidate_rows = ChatMessageModelEntity::find();
	        if let Some(cid) = chat_id {
	            candidate_rows =
	                candidate_rows.filter(crate::entities::ChatMessageColumn::ChatId.eq(cid.0.clone()));
	        } else if let Some(pid) = project_id {
	            // Filter messages by joining on chats for project constraint.
	            candidate_rows = candidate_rows.filter(
	                crate::entities::ChatMessageColumn::ChatId.in_subquery(
	                    ChatModelEntity::find()
	                        .select()
	                        .column(crate::entities::ChatColumn::Id)
	                        .filter(crate::entities::ChatColumn::ProjectId.eq(pid.to_string()))
	                            .as_query().clone(),
	                ),
	            );
	        }
	        let rows = candidate_rows.all(&self.conn).await?;
	        // Convert DB rows to in-memory messages for downstream embedding steps.
	        let messages: Vec<ChatMessage> = rows.iter().map(chat_message_from_model).collect();

	        // Lexical scores.
	        let lexical = match mode {
	            RetrievalMode::Bm25 | RetrievalMode::Hybrid => {
	                self.bm25_index.search(query, top_k * 4) // widen candidate set for hybrid rerank
	            }
	            RetrievalMode::Embedding => Vec::new(),
	        };
	            let lexical_map =
		            std::collections::HashMap::<MessageId, f32>::from_iter(lexical.into_iter());

	        // Embedding similarity (query vs message vectors).
	        let mut embedding_map = std::collections::HashMap::<MessageId, f32>::new();
	        if matches!(mode, RetrievalMode::Embedding | RetrievalMode::Hybrid) {
	            if let Some(query_vec) = self.embed_query(query).await? {
	                // Ensure embeddings exist for these messages (compute missing ones).
	                let newly = self.embed_messages_if_needed(&messages).await?;
	                // Fetch stored vectors.
	                let mut vectors = self
	                    .fetch_message_vectors(&messages.iter().map(|m| m.id.clone()).collect::<Vec<_>>())
	                    .await?;
	                // Merge newly computed vectors (which embed_messages_if_needed already persisted).
	                for (mid, vec) in newly {
	                    vectors.insert(mid, vec);
	                }
	                // Compute cosine similarity for each message.
	                for msg in &messages {
	                    if let Some(vec) = vectors.get(&msg.id) {
	                        if !vec.is_empty() && vec.len() == query_vec.len() {
	                            let sim = cosine_similarity(&query_vec, vec);
	                            if sim.is_finite() {
	                                embedding_map.insert(msg.id.clone(), sim);
	                            }
	                        }
	                    }
	                }
	            }
	        }

	        // Combine scores.
	        let mut combined = Vec::<(ChatMessage, f32)>::new();
	        for msg in messages {
	            let lid = lexical_map.get(&msg.id).copied().unwrap_or(0.0);
	            let eid = embedding_map.get(&msg.id).copied().unwrap_or(0.0);
	            let score = match mode {
	                RetrievalMode::Bm25 => lid,
	                RetrievalMode::Embedding => eid,
	                RetrievalMode::Hybrid => crate::fuse_scores(eid, lid, alpha),
	            };
	            if score > 0.0 {
	                combined.push((msg, score));
	            }
	        }
	        combined.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
	        combined.truncate(top_k);
	        Ok(combined)
	    }
}

/// Cosine similarity helper (no allocation).
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0;
    let mut norm_a = 0.0;
    let mut norm_b = 0.0;
    for i in 0..a.len() {
        let x = a[i];
        let y = b[i];
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    let denom = (norm_a.sqrt() * norm_b.sqrt()).max(1e-8);
    dot / denom
}

/// Encode a slice of f32 embeddings into little-endian bytes.
fn encode_embedding_vector(vec: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vec.len() * 4);
    for &v in vec {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    bytes
}

/// Decode little-endian f32 bytes back into a vector.
fn decode_embedding_vector(bytes: &[u8]) -> anyhow::Result<Vec<f32>> {
    if bytes.len() % 4 != 0 {
        return Err(anyhow::anyhow!("embedding byte length not divisible by 4"));
    }
    let mut out = Vec::with_capacity(bytes.len() / 4);
    for chunk in bytes.chunks(4) {
        let mut arr = [0u8; 4];
        arr.copy_from_slice(chunk);
        out.push(f32::from_le_bytes(arr));
    }
    Ok(out)
}

/// Persist (model,digest,vector) if not already stored.
async fn persist_embedding_vector(
    conn: &sea_orm::DatabaseConnection,
    model: &str,
    digest: &[u8],
    vector: &[f32],
) -> anyhow::Result<()> {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, ActiveModelTrait, Set};
    use crate::entities::{EmbeddingModelEntity, EmbeddingColumn, EmbeddingActiveModel};
    let existing = EmbeddingModelEntity::find()
        .filter(EmbeddingColumn::Model.eq(model.to_string()))
        .filter(EmbeddingColumn::Digest.eq(digest.to_vec()))
        .one(conn)
        .await?;
    if existing.is_some() {
        return Ok(());
    }
    let active = EmbeddingActiveModel {
        model: Set(model.to_string()),
        digest: Set(digest.to_vec()),
        dimensions: Set(encode_embedding_vector(vector)),
        created_at: Set(time::OffsetDateTime::now_utc()),
    };
    active.insert(conn).await?;
    Ok(())
}

/// Fetch embedding vector for (model,digest) if present.
async fn fetch_embedding_vector(
    conn: &sea_orm::DatabaseConnection,
    model: &str,
    digest: &[u8],
) -> anyhow::Result<Option<Vec<f32>>> {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use crate::entities::{EmbeddingModelEntity, EmbeddingColumn};
    if let Some(row) = EmbeddingModelEntity::find()
        .filter(EmbeddingColumn::Model.eq(model.to_string()))
        .filter(EmbeddingColumn::Digest.eq(digest.to_vec()))
        .one(conn)
        .await?
    {
        return Ok(Some(decode_embedding_vector(&row.dimensions)?));
    }
    Ok(None)
}

/// Creates initial chat metadata for a new chat.
pub fn new_chat_metadata(
    chat_id: ChatId,
    project_id: Option<String>,
    title: Option<String>,
    backend_kind: &EmbeddingBackendKind,
    config: &ChatHistoryConfig,
) -> ChatMetadata {
    let now = OffsetDateTime::now_utc();
    ChatMetadata {
        chat_id,
        project_id,
        title,
        summary: None,
        created_at: now,
        updated_at: now,
        total_messages: 0,
        total_characters: 0,
        summary_refreshed_characters: 0,
        token_estimate: 0,
        embedding_model: Some(resolve_embedding_model_name(backend_kind, &config.embedding_model)),
        archived: false,
        pinned: false,
        tags: Vec::new(),
        chat_vector: None,
    }
}

/// Creates chat message struct prior to persistence.
pub fn new_chat_message(
    chat_id: &ChatId,
    role: MessageRole,
    content: String,
) -> ChatMessage {
    ChatMessage {
        id: MessageId(format!("msg_{}", OffsetDateTime::now_utc().unix_timestamp_nanos())),
        chat_id: chat_id.clone(),
        role,
        content,
        created_at: OffsetDateTime::now_utc(),
        token_estimate: 0,
        tags: Vec::new(),
        embedding_digest: None,
    }
}
/// Simple local embedding store wrapper for fastembed fallback + caching.
/// For remote (OpenAI/Azure) we delegate to embedding_backend directly.
pub struct EmbeddingStore {
    _model_name: String,
    cache: crate::fastembed_cache::FastEmbedCache,
}

impl EmbeddingStore {
    pub fn new(model_name: String) -> Self {
        let model_enum = crate::fastembed_cache::FastEmbedCache::resolve_model_name(&model_name);
        Self {
            _model_name: model_name,
            cache: crate::fastembed_cache::FastEmbedCache::new(model_enum),
        }
    }

    pub fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.cache.embed(texts)
    }
}

/// Extended methods for ChatHistoryDb to integrate embeddings.
impl ChatHistoryDb {
    /// Embed a batch of messages' content (local fastembed path) and persist digests.
    pub async fn embed_messages_if_needed(
        &self,
        messages: &[ChatMessage],
    ) -> Result<HashMap<MessageId, Vec<f32>>> {
            let mut to_compute: Vec<(MessageId, String, Vec<u8>)> = Vec::new();
            for m in messages {
                let digest = compute_digest(&m.content);
                let existing = self.get_message_embedding_digest(&m.id).await?;
                if existing.is_none() {
                    to_compute.push((m.id.clone(), m.content.clone(), digest));
                }
            }
            if to_compute.is_empty() {
                return Ok(HashMap::new());
            }
            let texts: Vec<String> = to_compute.iter().map(|(_, c, _)| c.clone()).collect();
            let vectors = match &self.config.embedding_backend {
                EmbeddingBackendKind::FastEmbedLocal { .. } => {
                    self.embedding_store.embed(&texts)?
                }
                // Delegate to external backend (stubbed).
                _ => self.embedding_backend.embed(&texts).await?,
            };

            // Persist message_embeddings rows and embedding vectors (deduplicated by (model,digest)).
            for ((msg_id, _content, digest), vec) in to_compute.iter().zip(vectors.iter()) {
                // Store association (message -> (model,digest))
                self.insert_message_embedding(msg_id, &self.config.embedding_model, digest).await?;
                // Store vector if not already present
                persist_embedding_vector(&self.conn, &self.config.embedding_model, digest, vec).await?;
            }

            // Return map of newly computed embeddings.
            let mut out = HashMap::new();
            for ((msg_id, _content, _digest), vec) in to_compute.into_iter().zip(vectors.into_iter()) {
                out.insert(msg_id, vec);
            }
            Ok(out)
        }

        /// Compute query embedding for retrieval (embedding or hybrid modes).
        pub async fn embed_query(&self, query: &str) -> Result<Option<Vec<f32>>> {
            match self.config.default_retrieval_mode {
                RetrievalMode::Bm25 => Ok(None),
                RetrievalMode::Embedding | RetrievalMode::Hybrid => {
                    let texts = vec![query.to_string()];
                    let vecs = match &self.config.embedding_backend {
                        EmbeddingBackendKind::FastEmbedLocal { .. } => {
                            self.embedding_store.embed(&texts)?
                        }
                        _ => self.embedding_backend.embed(&texts).await?,
                    };
                    Ok(vecs.into_iter().next())
                }
            }
        }


    }

impl ChatHistoryDb {
    pub async fn fetch_message_vectors(
        &self,
        message_ids: &[MessageId],
    ) -> Result<std::collections::HashMap<MessageId, Vec<f32>>> {
            // removed unused sea_orm imports (FromQueryResult, Statement, DatabaseBackend, Value)
            if message_ids.is_empty() {
                return Ok(std::collections::HashMap::new());
            }
            // Load message -> (model,digest)
            let mapping_rows = MessageEmbeddingModelEntity::find()
                .filter(crate::entities::MessageEmbeddingColumn::MessageId.is_in(
                    message_ids.iter().map(|m| m.0.clone()),
                ))
                .all(&self.conn)
                .await?;
            if mapping_rows.is_empty() {
                return Ok(std::collections::HashMap::new());
            }

            // Map (model,digest) -> all message ids sharing that digest.
            let mut digest_map: std::collections::HashMap<(String, Vec<u8>), Vec<MessageId>> =
                std::collections::HashMap::new();
            for r in &mapping_rows {
                digest_map
                    .entry((r.model.clone(), r.digest.clone()))
                    .or_default()
                    .push(MessageId(r.message_id.clone()));
            }


            // removed unused EmbRow struct

            let _backend = self.conn.get_database_backend();
            let mut result = std::collections::HashMap::<MessageId, Vec<f32>>::new();

            // Fetch vectors via helper; avoids backend-specific raw SQL construction here.
            for ((model, digest), msg_ids) in digest_map {
                if let Ok(opt_vec) = fetch_embedding_vector(&self.conn, &model, &digest).await {
                    if let Some(vec) = opt_vec {
                        for mid in msg_ids {
                            result.insert(mid, vec.clone());
                        }
                    }
                }
            }

            Ok(result)
        }

        pub async fn recompute_chat_embedding(
            &self,
            chat_id: &ChatId,
        ) -> Result<Option<Vec<f32>>> {
            let (meta, messages) = self.fetch_chat_with_messages(chat_id).await?;
            if messages.is_empty() {
                return Ok(None);
            }

            let newly = self.embed_messages_if_needed(&messages).await?;
            let mut vectors_map = self
                .fetch_message_vectors(
                    &messages.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
                )
                .await?;

            for (mid, vec) in newly {
                vectors_map.insert(mid, vec);
            }

            let vectors: Vec<Vec<f32>> = vectors_map
                .into_iter()
                .filter_map(|(_, v)| if v.is_empty() { None } else { Some(v) })
                .collect();

            if vectors.is_empty() {
                return Ok(None);
            }

            let pooled = self.compute_chat_embedding(&vectors);
            if let Some(vec) = &pooled {
                let mut updated = meta.clone();
                updated.chat_vector = Some(vec.clone());
                updated.updated_at = OffsetDateTime::now_utc();
                self.update_chat(&updated).await?;
            }

            Ok(pooled)
        }
    }
