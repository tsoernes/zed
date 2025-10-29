-- Migration: create chat history tables (chats, chat_messages, message_embeddings, chat_tags).
-- Rationale (only non-obvious parts explained):
-- * tags stored in separate table (chat_tags) for portability across SQLite/Postgres.
-- * chat_vector stored as BLOB to avoid dialect differences for float arrays.
-- * message_embeddings maps messages to existing global embeddings table (digest+model) without duplicating vectors.

-- NOTE: TIMESTAMPTZ is used; on SQLite SeaORM will adapt (or use TEXT). If dialect differences arise,
-- future migrations may adjust types.

-- Up -----------------------------------------------------------------------

CREATE TABLE chats (
    id TEXT PRIMARY KEY,
    project_id TEXT NULL,
    title TEXT NULL,
    summary TEXT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    total_messages INTEGER NOT NULL DEFAULT 0,
    total_characters INTEGER NOT NULL DEFAULT 0,
    token_estimate INTEGER NOT NULL DEFAULT 0,
    embedding_model TEXT NULL,
    archived BOOLEAN NOT NULL DEFAULT FALSE,
    pinned BOOLEAN NOT NULL DEFAULT FALSE,
    summary_refreshed_characters INTEGER NOT NULL DEFAULT 0,
    chat_vector BLOB NULL -- mean-pooled embedding of messages; NULL until computed
);

-- Separate tags table to avoid array / JSON dependency differences.
CREATE TABLE chat_tags (
    chat_id TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    tag TEXT NOT NULL,
    PRIMARY KEY (chat_id, tag)
);

CREATE TABLE chat_messages (
    id TEXT PRIMARY KEY,
    chat_id TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    token_estimate INTEGER NOT NULL DEFAULT 0,
    embedding_digest BYTEA NULL -- hash of normalized content; reused for embedding lookup
);

-- Maps a message to an embedding digest + model; embedding vectors themselves live in existing embeddings table.
CREATE TABLE message_embeddings (
    message_id TEXT PRIMARY KEY REFERENCES chat_messages(id) ON DELETE CASCADE,
    model TEXT NOT NULL,
    digest BYTEA NOT NULL
);

-- Indexes chosen for common query patterns (list chats by project recency; fetch messages by chat and chronological order).
CREATE INDEX chats_project_updated_idx ON chats(project_id, updated_at DESC);
CREATE INDEX chat_messages_chat_id_created_idx ON chat_messages(chat_id, created_at);

-- Uniqueness for (model,digest) lookups to reuse vectors (digest uniqueness scoped per model).
CREATE UNIQUE INDEX message_embeddings_model_digest_idx ON message_embeddings(model, digest);

-- Down ---------------------------------------------------------------------
-- Provide down migration in case rollback is needed.

-- The down section will drop all created objects. Order matters due to foreign keys.

-- Down: drop indexes explicitly only if required (some dialects auto-drop with table).
-- Using IF EXISTS for safety during partial rollbacks.

-- Down ---------------------------------------------------------------------
-- (Wrap drops in a transaction handled by migration runner if available.)

-- DROP ORDER (children first):
-- message_embeddings -> chat_messages -> chat_tags -> chats

DROP TABLE IF EXISTS message_embeddings;
DROP TABLE IF EXISTS chat_messages;
DROP TABLE IF EXISTS chat_tags;
DROP TABLE IF EXISTS chats;
