-- Chat History Initial Schema Migration
-- ---------------------------------------------------------------------------
-- This migration creates the persistence layer for chat history:
--   * chats                : per-chat metadata and aggregate stats
--   * chat_messages        : individual messages inside a chat
--   * chat_tags            : many-to-many tags (simple association)
--   * embeddings           : global cache of unique embedding vectors
--   * message_embeddings   : mapping message -> (model,digest) for embedding reuse
--
-- Notes:
-- - Vector fields are stored as raw little-endian f32 bytes (BLOB/TEXT depending on dialect).
-- - Using INTEGER / TEXT / BLOB types keeps compatibility with SQLite; Postgres may
--   coerce BLOB to BYTEA automatically depending on driver. Adjust in future dialect-
--   specific migrations if needed.
-- - All timestamps stored as TEXT (ISO8601) or TIMESTAMP depending on the underlying DB.
-- - No IF NOT EXISTS clauses: migrations are applied exactly once by the migration runner.
-- - Foreign keys use ON DELETE CASCADE to simplify cleanup when a chat is removed.
--
-- Future migration considerations:
--   * Add per-project composite indices for faster project-scoped queries.
--   * Add partial indices for archived / pinned filtering.
--   * Normalize tags into separate table if cardinality grows significantly.
--   * Introduce a revisions table for summarization evolution if needed.
-- ---------------------------------------------------------------------------

-- Chats table: core metadata + aggregate stats + optional pooled embedding vector.
CREATE TABLE chats (
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
    pinned   BOOLEAN NOT NULL DEFAULT FALSE,
    chat_vector BLOB NULL
);

-- Index to accelerate listing chats within a project ordered by updated time.
CREATE INDEX idx_chats_project_updated ON chats (project_id, updated_at);

-- Tags: simple association (chat_id, tag).
CREATE TABLE chat_tags (
    chat_id TEXT NOT NULL,
    tag     TEXT NOT NULL,
    PRIMARY KEY (chat_id, tag),
    FOREIGN KEY (chat_id) REFERENCES chats(id) ON DELETE CASCADE
);

-- Optional index for reverse lookups by tag (list all chats with a tag).
CREATE INDEX idx_chat_tags_tag ON chat_tags (tag);

-- Messages inside chats.
CREATE TABLE chat_messages (
    id TEXT PRIMARY KEY,
    chat_id TEXT NOT NULL,
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    created_at TEXT NOT NULL,
    token_estimate INTEGER NOT NULL DEFAULT 0,
    embedding_digest BLOB NULL,
    FOREIGN KEY (chat_id) REFERENCES chats(id) ON DELETE CASCADE
);

-- Index to accelerate retrieval of messages in chronological order per chat.
CREATE INDEX idx_chat_messages_chat_created ON chat_messages (chat_id, created_at);

-- Global embedding cache keyed by (model,digest).
CREATE TABLE embeddings (
    model TEXT NOT NULL,
    digest BLOB NOT NULL,
    dimensions BLOB NOT NULL,      -- Raw little-endian f32 bytes encoding dimension vector length (implementation-specific).
    created_at TEXT NOT NULL,
    PRIMARY KEY (model, digest)
);

-- Index to allow scanning embeddings by model quickly (optional optimization).
CREATE INDEX idx_embeddings_model ON embeddings (model);

-- Per-message embedding association.
-- Digest here matches the digest stored in embeddings for the same model.
CREATE TABLE message_embeddings (
    message_id TEXT PRIMARY KEY,
    model TEXT NOT NULL,
    digest BLOB NOT NULL,
    FOREIGN KEY (message_id) REFERENCES chat_messages(id) ON DELETE CASCADE
);

-- Index to support queries for messages pending embedding operations / model changes.
CREATE INDEX idx_message_embeddings_model ON message_embeddings (model);

-- ---------------------------------------------------------------------------
-- End of migration.
-- ---------------------------------------------------------------------------
