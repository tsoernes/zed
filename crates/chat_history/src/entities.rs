use anyhow::{anyhow, Result};
use sea_orm::{entity::prelude::*, ActiveValue};
use time::OffsetDateTime;

use super::{ChatId, ChatMessage, ChatMetadata, EmbeddingBackendKind, MessageId, MessageRole};

/// Storing chat_vector as raw little-endian f32 bytes to avoid dialect-specific float array types.
/// This allows consistent decoding regardless of Postgres / SQLite differences.
pub fn encode_chat_vector(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vector.len() * 4);
    for v in vector {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    bytes
}

pub fn decode_chat_vector(bytes: &[u8]) -> Result<Vec<f32>> {
    if bytes.len() % 4 != 0 {
        return Err(anyhow!("chat_vector byte length not divisible by 4"));
    }
    let mut out = Vec::with_capacity(bytes.len() / 4);
    for chunk in bytes.chunks(4) {
        let mut arr = [0u8; 4];
        arr.copy_from_slice(chunk);
        out.push(f32::from_le_bytes(arr));
    }
    Ok(out)
}

// -----------------------------------------------------------------------------
// Chats
// -----------------------------------------------------------------------------
pub mod chats {
    use super::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "chats")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: String,
        pub project_id: Option<String>,
        pub title: Option<String>,
        pub summary: Option<String>,
        pub created_at: OffsetDateTime,
        pub updated_at: OffsetDateTime,
        pub total_messages: i32,
        pub total_characters: i32,
        pub summary_refreshed_characters: i32,
        pub token_estimate: i32,
        pub embedding_model: Option<String>,
        pub archived: bool,
        pub pinned: bool,
        pub chat_vector: Option<Vec<u8>>,
    }

    #[derive(Copy, Clone, Debug, EnumIter)]
    pub enum Relation {
        Messages,
        Tags,
    }

    impl RelationTrait for Relation {
        fn def(&self) -> RelationDef {
            match self {
                // chats has many chat_messages
                Relation::Messages => Entity::has_many(super::chat_messages::Entity).into(),
                // chats has many chat_tags
                Relation::Tags => Entity::has_many(super::chat_tags::Entity).into(),
            }
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

// -----------------------------------------------------------------------------
// Chat Messages
// -----------------------------------------------------------------------------
pub mod chat_messages {
    use super::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "chat_messages")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: String,
        pub chat_id: String,
        pub role: String,
        pub content: String,
        pub created_at: OffsetDateTime,
        pub token_estimate: i32,
        pub embedding_digest: Option<Vec<u8>>,
    }

    #[derive(Copy, Clone, Debug, EnumIter)]
    pub enum Relation {
        Chat,
    }

    impl RelationTrait for Relation {
        fn def(&self) -> RelationDef {
            match self {
                Relation::Chat => Entity::belongs_to(super::chats::Entity)
                    .from(Column::ChatId)
                    .to(super::chats::Column::Id)
                    .into(),
            }
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

// -----------------------------------------------------------------------------
// Chat Tags
// -----------------------------------------------------------------------------
pub mod chat_tags {
    use super::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "chat_tags")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub chat_id: String,
        #[sea_orm(primary_key)]
        pub tag: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter)]
    pub enum Relation {
        Chat,
    }

    impl RelationTrait for Relation {
        fn def(&self) -> RelationDef {
            match self {
                Relation::Chat => Entity::belongs_to(super::chats::Entity)
                    .from(Column::ChatId)
                    .to(super::chats::Column::Id)
                    .into(),
            }
        }
    }

    impl ActiveModelBehavior for ActiveModel {}

    // Implement Related so has_many works for chats -> chat_messages / chat_tags.
    impl Related<chats::Entity> for chat_messages::Entity {
        fn to() -> RelationDef {
            chat_messages::Relation::Chat.def()
        }
    }

    impl Related<chats::Entity> for chat_tags::Entity {
        fn to() -> RelationDef {
            chat_tags::Relation::Chat.def()
        }
    }

    // (Removed extraneous impl Related<chats::Entity> blocks for chat_messages and chat_tags to avoid confusion.)
}

// -----------------------------------------------------------------------------
// Embeddings (vector storage)
// -----------------------------------------------------------------------------
pub mod embeddings {
    use super::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "embeddings")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub model: String,
        #[sea_orm(primary_key)]
        pub digest: Vec<u8>,
        pub dimensions: Vec<u8>,
        pub created_at: OffsetDateTime,
    }

    #[derive(Copy, Clone, Debug, EnumIter)]
    pub enum Relation {}

    impl RelationTrait for Relation {
        fn def(&self) -> RelationDef {
            // Embeddings has no outbound relations; unreachable by design.
            match self {
                _ => unreachable!("embeddings::Relation has no variants"),
            }
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

// -----------------------------------------------------------------------------
// Message Embeddings (associations message -> (model,digest))
// -----------------------------------------------------------------------------
pub mod message_embeddings {
    use super::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "message_embeddings")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub message_id: String,
        pub model: String,
        pub digest: Vec<u8>,
    }

    #[derive(Copy, Clone, Debug, EnumIter)]
    pub enum Relation {
        Message,
        Embedding,
    }

    impl RelationTrait for Relation {
        fn def(&self) -> RelationDef {
            match self {
                Relation::Message => Entity::belongs_to(super::chat_messages::Entity)
                    .from(Column::MessageId)
                    .to(super::chat_messages::Column::Id)
                    .into(),
                Relation::Embedding => Entity::belongs_to(super::embeddings::Entity)
                    .from(Column::Model)
                    .to(super::embeddings::Column::Model)
                    .into(),
            }
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

// -----------------------------------------------------------------------------
// Re-exports (aliases) to keep previous external names stable
// -----------------------------------------------------------------------------
pub use chat_messages::{
    ActiveModel as ChatMessageActiveModel, Column as ChatMessageColumn,
    Entity as ChatMessageModelEntity, Model as ChatMessageModel,
};
pub use chat_tags::{
    ActiveModel as ChatTagActiveModel, Column as ChatTagColumn, Entity as ChatTagModelEntity,
    Model as ChatTagModel,
};
pub use chats::{
    ActiveModel as ChatActiveModel, Column as ChatColumn, Entity as ChatModelEntity,
    Model as ChatModel,
};
pub use embeddings::{
    ActiveModel as EmbeddingActiveModel, Column as EmbeddingColumn, Entity as EmbeddingModelEntity,
    Model as EmbeddingModel,
};
pub use message_embeddings::{
    ActiveModel as MessageEmbeddingActiveModel, Column as MessageEmbeddingColumn,
    Entity as MessageEmbeddingModelEntity, Model as MessageEmbeddingModel,
};

/// Convert DB chat model + collected tags into in-memory metadata.
pub fn chat_metadata_from_model(model: &ChatModel, tags: &[String]) -> Result<ChatMetadata> {
    let chat_vector = if let Some(bytes) = &model.chat_vector {
        Some(decode_chat_vector(bytes)?)
    } else {
        None
    };

    Ok(ChatMetadata {
        chat_id: ChatId(model.id.clone()),
        project_id: model.project_id.clone(),
        title: model.title.clone(),
        summary: model.summary.clone(),
        created_at: model.created_at,
        updated_at: model.updated_at,
        total_messages: model.total_messages as usize,
        total_characters: model.total_characters as usize,
        summary_refreshed_characters: model.summary_refreshed_characters as usize,
        token_estimate: model.token_estimate as usize,
        embedding_model: model.embedding_model.clone(),
        archived: model.archived,
        pinned: model.pinned,
        tags: tags.to_vec(),
        chat_vector,
    })
}

/// Convert DB message model into in-memory ChatMessage.
pub fn chat_message_from_model(model: &ChatMessageModel) -> ChatMessage {
    ChatMessage {
        id: MessageId(model.id.clone()),
        chat_id: ChatId(model.chat_id.clone()),
        role: map_role_string(&model.role),
        content: model.content.clone(),
        created_at: model.created_at,
        token_estimate: model.token_estimate as usize,
        tags: Vec::new(),
        embedding_digest: model.embedding_digest.clone(),
    }
}

/// Decide on role mapping; unknown roles preserved to avoid losing data.
fn map_role_string(role: &str) -> MessageRole {
    match role {
        "user" => MessageRole::User,
        "assistant" => MessageRole::Assistant,
        "system" => MessageRole::System,
        "tool" => MessageRole::Tool,
        other => MessageRole::Other(other.to_string()),
    }
}

/// Prepare active model for insertion based on ChatMetadata (chat_vector converted).
pub fn active_chat_from_metadata(meta: &ChatMetadata) -> ChatActiveModel {
    ChatActiveModel {
        id: ActiveValue::set(meta.chat_id.0.clone()),
        project_id: ActiveValue::set(meta.project_id.clone()),
        title: ActiveValue::set(meta.title.clone()),
        summary: ActiveValue::set(meta.summary.clone()),
        created_at: ActiveValue::set(meta.created_at),
        updated_at: ActiveValue::set(meta.updated_at),
        total_messages: ActiveValue::set(meta.total_messages as i32),
        total_characters: ActiveValue::set(meta.total_characters as i32),
        summary_refreshed_characters: ActiveValue::set(meta.summary_refreshed_characters as i32),
        token_estimate: ActiveValue::set(meta.token_estimate as i32),
        embedding_model: ActiveValue::set(meta.embedding_model.clone()),
        archived: ActiveValue::set(meta.archived),
        pinned: ActiveValue::set(meta.pinned),
        chat_vector: ActiveValue::set(meta.chat_vector.as_ref().map(|v| encode_chat_vector(v))),
    }
}

/// Prepare active model for insertion for a message.
pub fn active_message_from_chat_message(msg: &ChatMessage) -> ChatMessageActiveModel {
    ChatMessageActiveModel {
        id: ActiveValue::set(msg.id.0.clone()),
        chat_id: ActiveValue::set(msg.chat_id.0.clone()),
        role: ActiveValue::set(role_string(&msg.role)),
        content: ActiveValue::set(msg.content.clone()),
        created_at: ActiveValue::set(msg.created_at),
        token_estimate: ActiveValue::set(msg.token_estimate as i32),
        embedding_digest: ActiveValue::set(msg.embedding_digest.clone()),
    }
}

fn role_string(role: &MessageRole) -> String {
    match role {
        MessageRole::User => "user".into(),
        MessageRole::Assistant => "assistant".into(),
        MessageRole::System => "system".into(),
        MessageRole::Tool => "tool".into(),
        MessageRole::Other(s) => s.clone(),
    }
}

/// Update existing chat active model with new metadata fields (other fields unchanged).
pub fn apply_metadata_update(active: &mut ChatActiveModel, meta: &ChatMetadata) {
    active.title = ActiveValue::set(meta.title.clone());
    active.summary = ActiveValue::set(meta.summary.clone());
    active.updated_at = ActiveValue::set(meta.updated_at);
    active.total_messages = ActiveValue::set(meta.total_messages as i32);
    active.total_characters = ActiveValue::set(meta.total_characters as i32);
    active.summary_refreshed_characters =
        ActiveValue::set(meta.summary_refreshed_characters as i32);
    active.token_estimate = ActiveValue::set(meta.token_estimate as i32);
    active.archived = ActiveValue::set(meta.archived);
    active.pinned = ActiveValue::set(meta.pinned);
    active.chat_vector = ActiveValue::set(meta.chat_vector.as_ref().map(|v| encode_chat_vector(v)));
}

/// Decide embedding model field from backend kind and explicit config.
pub fn resolve_embedding_model_name(kind: &EmbeddingBackendKind, configured: &str) -> String {
    match kind {
        EmbeddingBackendKind::FastEmbedLocal { .. } => configured.to_string(),
        EmbeddingBackendKind::OpenAI { model, .. } => model.clone(),
        EmbeddingBackendKind::AzureOpenAI { model, .. } => model.clone(),
    }
}

// Backwards-compatible entity aliases (for external references expecting old names).
pub type ChatModelEntityAlias = chats::Entity;
pub type ChatMessageModelEntityAlias = chat_messages::Entity;
pub type ChatTagModelEntityAlias = chat_tags::Entity;
pub type MessageEmbeddingModelEntityAlias = message_embeddings::Entity;
// Re-export original entity names for compatibility with existing db.rs references.

// Type aliases for SeaORM generated entities for clarity.
