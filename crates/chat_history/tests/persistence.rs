use std::sync::Arc;

use anyhow::Result;
use chat_history::{
    ChatHistoryConfig, ChatId, ChatMessage, ChatMetadata, ChatStore, EmbeddingBackendKind,
    MessageRole, db::ChatHistoryDb,
};
use sea_orm::{Database, DatabaseConnection};
use time::OffsetDateTime;

/// Minimal DDL for tests (SQLite in-memory). We mirror the migration definitions needed
/// by the entities used in ChatHistoryDb; only fields required by current code paths are included.
/// NOTE: Using IF NOT EXISTS to allow idempotent calls if tests run in isolation.
async fn create_schema(conn: &DatabaseConnection) -> Result<()> {
    // chats
    conn.execute_unprepared(
        r#"
        CREATE TABLE IF NOT EXISTS chats (
            id TEXT PRIMARY KEY,
            project_id TEXT NULL,
            title TEXT NULL,
            summary TEXT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            total_messages INTEGER NOT NULL DEFAULT 0,
            total_characters INTEGER NOT NULL DEFAULT 0,
            summary_refreshed_characters INTEGER NOT NULL DEFAULT 0,
            token_estimate INTEGER NOT NULL DEFAULT 0,
            embedding_model TEXT NULL,
            archived BOOLEAN NOT NULL DEFAULT FALSE,
            pinned BOOLEAN NOT NULL DEFAULT FALSE,
            chat_vector BLOB NULL
        );
        "#,
    )
    .await?;

    // chat_tags
    conn.execute_unprepared(
        r#"
        CREATE TABLE IF NOT EXISTS chat_tags (
            chat_id TEXT NOT NULL,
            tag TEXT NOT NULL,
            PRIMARY KEY (chat_id, tag),
            FOREIGN KEY (chat_id) REFERENCES chats(id) ON DELETE CASCADE
        );
        "#,
    )
    .await?;

    // chat_messages
    conn.execute_unprepared(
        r#"
        CREATE TABLE IF NOT EXISTS chat_messages (
            id TEXT PRIMARY KEY,
            chat_id TEXT NOT NULL,
            role TEXT NOT NULL,
            content TEXT NOT NULL,
            created_at TEXT NOT NULL,
            token_estimate INTEGER NOT NULL DEFAULT 0,
            embedding_digest BLOB NULL,
            FOREIGN KEY (chat_id) REFERENCES chats(id) ON DELETE CASCADE
        );
        "#,
    )
    .await?;

    // message_embeddings
    conn.execute_unprepared(
        r#"
        CREATE TABLE IF NOT EXISTS message_embeddings (
            message_id TEXT PRIMARY KEY,
            model TEXT NOT NULL,
            digest BLOB NOT NULL,
            FOREIGN KEY (message_id) REFERENCES chat_messages(id) ON DELETE CASCADE
        );
        "#,
    )
    .await?;

    // embeddings (global embedding cache)
    conn.execute_unprepared(
        r#"
        CREATE TABLE IF NOT EXISTS embeddings (
            model TEXT NOT NULL,
            digest BLOB NOT NULL,
            dimensions BLOB NOT NULL,
            created_at TEXT NOT NULL,
            PRIMARY KEY (model, digest)
        );
        "#,
    )
    .await?;

    Ok(())
}

/// Helper to build a ChatStore with a DB-backed ChatHistoryDb.
async fn build_store_with_db(conn: DatabaseConnection) -> Result<ChatStore> {
    let mut config = ChatHistoryConfig::default();
    // Force fastembed local backend to avoid external network access.
    config.embedding_backend = EmbeddingBackendKind::FastEmbedLocal { model_path: None };

    // FastEmbed backend chosen by config + store initializer path.
    let backend_kind = config.embedding_backend.clone();
    // Use a simple local backend (reuse embedding_model from config).
    let backend: Arc<dyn chat_history::EmbeddingBackend> = match backend_kind {
        EmbeddingBackendKind::FastEmbedLocal { .. } => {
            Arc::new(chat_history::FastEmbedBackend::new(&config.embedding_model))
        }
        EmbeddingBackendKind::OpenAI { .. } => {
            // Should not occur in test configuration, but handle gracefully.
            Arc::new(chat_history::OpenAIEmbeddingBackend::new(
                config.embedding_model.clone(),
            ))
        }
        EmbeddingBackendKind::AzureOpenAI { .. } => Arc::new(
            chat_history::AzureOpenAIEmbeddingBackend::new(config.embedding_model.clone()),
        ),
    };

    let db = ChatHistoryDb::new(conn, config.clone(), backend.clone());
    let store = ChatStore::new(backend, config, Some(Arc::new(db)));
    Ok(store)
}

/// Create a new chat metadata object without persisting messages (since ChatStore::append_message
/// currently persists only chat metadata and tags).
async fn create_chat_and_append_message(store: &ChatStore) -> Result<ChatMetadata> {
    let mut chat = store
        .create_chat(Some("proj-1".into()), Some("Initial Chat".into()))
        .await?;
    let _msg = store
        .append_message(&mut chat, MessageRole::User, "Hello persistence!".into())
        .await?;
    Ok(chat)
}

/// Verify that after append_message the chat row exists and metadata fields are consistent.
#[tokio::test]
async fn test_chat_insert_and_fetch() -> Result<()> {
    let conn = Database::connect("sqlite::memory:").await?;
    create_schema(&conn).await?;
    let store = build_store_with_db(conn).await?;

    let chat_meta = create_chat_and_append_message(&store).await?;
    assert_eq!(chat_meta.total_messages, 1);
    assert!(chat_meta.total_characters > 0);

    // Fetch via API (should succeed because DB is present).
    let (fetched_meta, messages) = store.get_chat(&chat_meta.chat_id).await?;
    assert_eq!(fetched_meta.chat_id, chat_meta.chat_id);
    assert_eq!(fetched_meta.total_messages, chat_meta.total_messages);
    assert_eq!(
        messages.len(),
        0,
        "Message persistence not yet implemented; expect 0"
    );
    Ok(())
}

/// Exercise update_metadata: add/remove tags, set summary & archived/pinned flags.
#[tokio::test]
async fn test_update_metadata_persists_changes() -> Result<()> {
    let conn = Database::connect("sqlite::memory:").await?;
    create_schema(&conn).await?;
    let store = build_store_with_db(conn).await?;

    let chat_meta = create_chat_and_append_message(&store).await?;
    let chat_id = chat_meta.chat_id.clone();

    // Apply metadata changes.
    let updated = store
        .update_metadata(
            &chat_id,
            Some("Renamed Chat".into()),
            Some("Short summary".into()),
            &["tagA".into(), "tagB".into()],
            &[],
            Some(true),
            Some(true),
        )
        .await?;

    assert_eq!(updated.title.as_deref(), Some("Renamed Chat"));
    assert_eq!(updated.summary.as_deref(), Some("Short summary"));
    assert!(updated.archived);
    assert!(updated.pinned);
    assert!(updated.tags.contains(&"tagA".to_string()));
    assert!(updated.tags.contains(&"tagB".to_string()));

    // Remove one tag and add a new one; ensure removal + addition reflected.
    let updated2 = store
        .update_metadata(
            &chat_id,
            None,
            None,
            &["tagC".into()],
            &["tagA".into()],
            None,
            None,
        )
        .await?;

    assert!(!updated2.tags.contains(&"tagA".to_string()));
    assert!(updated2.tags.contains(&"tagB".to_string()));
    assert!(updated2.tags.contains(&"tagC".to_string()));

    // Summary was set previously; verify summary_refreshed_characters recorded.
    assert_eq!(
        updated.summary_refreshed_characters, updated.total_characters,
        "Summary refresh characters should match total after summary update"
    );

    Ok(())
}

/// (Optional future test) Ensure that once message persistence is implemented,
/// appended messages appear in `get_chat`. For now we assert absence.
/// This test is ignored until message persistence is added.
#[tokio::test]
#[ignore]
async fn test_message_persistence_future() -> Result<()> {
    let conn = Database::connect("sqlite::memory:").await?;
    create_schema(&conn).await?;
    let store = build_store_with_db(conn).await?;
    let chat_meta = create_chat_and_append_message(&store).await?;
    let (_meta_again, messages) = store.get_chat(&chat_meta.chat_id).await?;
    assert!(
        !messages.is_empty(),
        "Once implemented, messages should be persisted and returned"
    );
    Ok(())
}

/// Utility to construct a dummy ChatMessage if future tests need message-level embedding logic.
/// Currently unused because ChatStore::append_message does not persist messages.
#[allow(dead_code)]
fn dummy_message(chat_id: &ChatId, content: &str, role: MessageRole) -> ChatMessage {
    ChatMessage {
        id: chat_history::MessageId(format!(
            "msg_{}",
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        )),
        chat_id: chat_id.clone(),
        role,
        content: content.to_string(),
        created_at: OffsetDateTime::now_utc(),
        token_estimate: 0,
        tags: Vec::new(),
        embedding_digest: None,
    }
}
