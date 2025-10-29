use std::sync::{Arc, OnceLock};

use action_log::ActionLog;
use anyhow::{anyhow, Result};
use assistant_tool::{Tool, ToolResult, ToolResultOutput};
use chat_history::{MessageRole, RetrievalMode};
use chat_history_tools::{init::ChatHistoryHandles, ChatHistoryToolApi, ChatHistoryTools};
use gpui::{AnyWindowHandle, App, Entity, Task};
use language_model::{LanguageModel, LanguageModelRequest, LanguageModelToolSchemaFormat};
use project::Project;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use ui::IconName;

/// Global storage for the chat history adapter.
/// This is initialized once via `install_chat_history_adapter`.
static CHAT_HISTORY_ADAPTER: OnceLock<Arc<ChatHistoryTools>> = OnceLock::new();

/// Install the chat history adapter produced externally (e.g. via `init_chat_history_tools`).
/// Must be invoked during startup before the tool can be used.
pub fn install_chat_history_adapter(handles: &ChatHistoryHandles) {
    // Ignore duplicate install attempts; first wins.
    let _ = CHAT_HISTORY_ADAPTER.set(handles.tools.clone());
}

/// Obtain the adapter or return an error if not yet installed.
fn adapter() -> Result<Arc<ChatHistoryTools>> {
    CHAT_HISTORY_ADAPTER
        .get()
        .cloned()
        .ok_or_else(|| anyhow!("chat history adapter not installed"))
}

/// High‑level operations exposed by the `chat_history` tool.
///
/// Each variant maps directly to a JSON method on `ChatHistoryTools`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatHistoryOperation {
    /// Append a message to a chat (creates chat if missing).
    Append {
        #[serde(default)]
        chat_id: Option<String>,
        #[serde(default)]
        project_id: Option<String>,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        role: Option<MessageRole>,
        content: String,
    },
    /// Keyword / semantic search over stored chats.
    Search {
        query: String,
        #[serde(default)]
        project_id: Option<String>,
        #[serde(default)]
        chat_id: Option<String>,
        #[serde(default)]
        top_k: Option<usize>,
        #[serde(default)]
        mode: Option<RetrievalMode>,
        #[serde(default)]
        alpha: Option<f32>,
    },
    /// RAG answer synthesis over prior chats.
    Answer {
        question: String,
        #[serde(default)]
        project_id: Option<String>,
        #[serde(default)]
        chat_id: Option<String>,
        #[serde(default)]
        top_k: Option<usize>,
        #[serde(default)]
        mode: Option<RetrievalMode>,
        #[serde(default)]
        alpha: Option<f32>,
    },
    /// Find similar chats to the given chat id.
    Similar {
        chat_id: String,
        #[serde(default)]
        n: Option<usize>,
        #[serde(default)]
        project_scoped: Option<bool>,
    },
    /// List chat metadata.
    List {
        #[serde(default)]
        project_id: Option<String>,
        #[serde(default)]
        limit: Option<usize>,
        #[serde(default)]
        offset: Option<usize>,
    },
    /// Get a chat (metadata + messages).
    Get {
        chat_id: String,
    },
    /// Recompute embeddings (optionally for one chat).
    Reembed {
        #[serde(default)]
        chat_id: Option<String>,
    },
    /// Update metadata fields / tags.
    UpdateMetadata {
        chat_id: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        summary: Option<String>,
        #[serde(default)]
        tags_add: Option<Vec<String>>,
        #[serde(default)]
        tags_remove: Option<Vec<String>>,
        #[serde(default)]
        archived: Option<bool>,
        #[serde(default)]
        pinned: Option<bool>,
    },
    /// Fetch current config (secrets redacted).
    ConfigGet,
    /// Set selected config fields.
    ConfigSet {
        #[serde(default)]
        embedding_model: Option<String>,
        #[serde(default)]
        hybrid_alpha: Option<f32>,
        #[serde(default)]
        similar_chats_k: Option<usize>,
        #[serde(default)]
        summary_refresh_chars: Option<usize>,
        #[serde(default)]
        summary_delta_chars: Option<usize>,
        #[serde(default)]
        rag_top_k: Option<usize>,
        #[serde(default)]
        auto_tag: Option<bool>,
        #[serde(default)]
        default_retrieval_mode: Option<RetrievalMode>,
    },
}

/// Tool input envelope.
#[derive(Debug, Serialize, Deserialize)]
pub struct ChatHistoryToolInput {
    pub operation: ChatHistoryOperation,
}

/// Native wrapper Tool exposing `ChatHistoryTools` adapter functionality.
pub struct ChatHistoryTool;

impl Tool for ChatHistoryTool {
    fn name(&self) -> String {
        "chat_history".into()
    }

    fn description(&self) -> String {
        // Expanded tool description for the LLM / agent:
        // - This tool exposes persistent chat history functionality backed by an embedding + BM25 hybrid store.
        // - Use Append to record a new message (creates the chat if chat_id is absent).
        // - Use Search for keyword / semantic retrieval (supports mode + alpha to tune BM25 vs embedding fusion).
        // - Use Answer to perform lightweight RAG synthesis over prior chats (returns an answer string).
        // - Use Similar to locate chats related to a given chat_id.
        // - Use List to enumerate stored chats with pagination.
        // - Use Get to fetch full messages + metadata for a single chat.
        // - Use Reembed to recompute embeddings (optionally for a single chat or all).
        // - Use UpdateMetadata to edit title / summary / tags / archived / pinned flags.
        // - Use ConfigGet / ConfigSet to inspect or adjust non-secret config fields (secrets redacted on get).
        // Guidance:
        // * Prefer Search then Answer (two-step) when you need explicit context objects before synthesis.
        // * Reembed should be used sparingly (confirmation required) after major model/config changes.
        // * Append should not be used to store extremely large blobs; chunk them or summarize first.
        // * Alpha controls hybrid weighting (0.0 = BM25 only, 1.0 = embedding only) when mode=Hybrid.
        // Error Handling:
        // * All adapter responses wrap {"ok": true|false}; failures surface as tool errors.
        // * Mutating operations (reembed, update metadata, config set) may require confirmation.
        "Persistent chat history operations: append, search, answer (RAG), list, get, similar, reembed, update metadata, config get/set."
            .into()
    }

    fn icon(&self) -> IconName {
        // Reuse an existing icon used for knowledge-like operations.
        IconName::Book
    }

    fn needs_confirmation(
        &self,
        input: &serde_json::Value,
        _project: &Entity<Project>,
        _cx: &App,
    ) -> bool {
        // Mutations that might have broader side effects.
        if let Ok(parsed) = serde_json::from_value::<ChatHistoryToolInput>(input.clone()) {
            matches!(
                parsed.operation,
                ChatHistoryOperation::Reembed { .. }
                    | ChatHistoryOperation::UpdateMetadata { .. }
                    | ChatHistoryOperation::ConfigSet { .. }
            )
        } else {
            false
        }
    }

    fn may_perform_edits(&self) -> bool {
        // Some operations change stored state.
        true
    }

    fn input_schema(&self, _format: LanguageModelToolSchemaFormat) -> Result<serde_json::Value> {
        // Simplified schema: single "operation" object with a string discriminator "type"
        // and a flat set of optional fields used by the various operation variants.
        // The tool runtime still expects a structured ChatHistoryToolInput, but the LLM
        // can supply only the needed fields for the chosen type.
        let schema = json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "object",
                    "properties": {
                        "type": {
                            "type": "string",
                            "enum": [
                                "append","search","answer","similar","list","get",
                                "reembed","update_metadata","config_get","config_set"
                            ]
                        },
                        "chat_id": { "type": "string" },
                        "project_id": { "type": "string" },
                        "title": { "type": "string" },
                        "role": { "type": "string", "description": "User|Assistant" },
                        "content": { "type": "string" },
                        "query": { "type": "string" },
                        "question": { "type": "string" },
                        "top_k": { "type": "integer" },
                        "mode": { "type": "string", "description": "bm25|embedding|hybrid" },
                        "alpha": { "type": "number" },
                        "n": { "type": "integer" },
                        "project_scoped": { "type": "boolean" },
                        "limit": { "type": "integer" },
                        "offset": { "type": "integer" },
                        "summary": { "type": "string" },
                        "tags_add": { "type": "array", "items": { "type": "string" } },
                        "tags_remove": { "type": "array", "items": { "type": "string" } },
                        "archived": { "type": "boolean" },
                        "pinned": { "type": "boolean" },
                        "embedding_model": { "type": "string" },
                        "hybrid_alpha": { "type": "number" },
                        "similar_chats_k": { "type": "integer" },
                        "summary_refresh_chars": { "type": "integer" },
                        "summary_delta_chars": { "type": "integer" },
                        "rag_top_k": { "type": "integer" },
                        "auto_tag": { "type": "boolean" },
                        "default_retrieval_mode": { "type": "string", "description": "bm25|embedding|hybrid" }
                    },
                    "required": ["type"]
                }
            },
            "required": ["operation"]
        });
        Ok(schema)
    }

    fn ui_text(&self, input: &serde_json::Value) -> String {
        let parsed: Result<ChatHistoryToolInput> = serde_json::from_value(input.clone()).map_err(|e| anyhow!(e));
        if let Ok(env) = parsed {
            match env.operation {
                ChatHistoryOperation::Append { .. } => "Append message".into(),
                ChatHistoryOperation::Search { query, .. } => format!("Search chats: {query}"),
                ChatHistoryOperation::Answer { question, .. } => {
                    format!("Answer from history: {question}")
                }
                ChatHistoryOperation::Similar { chat_id, .. } => {
                    format!("Similar chats to {chat_id}")
                }
                ChatHistoryOperation::List { .. } => "List chats".into(),
                ChatHistoryOperation::Get { chat_id } => format!("Get chat {chat_id}"),
                ChatHistoryOperation::Reembed { chat_id } => match chat_id {
                    Some(id) => format!("Reembed chat {id}"),
                    None => "Reembed all chats".into(),
                },
                ChatHistoryOperation::UpdateMetadata { chat_id, .. } => {
                    format!("Update metadata for {chat_id}")
                }
                ChatHistoryOperation::ConfigGet => "Get chat history config".into(),
                ChatHistoryOperation::ConfigSet { .. } => "Set chat history config".into(),
            }
        } else {
            "Chat history operation".into()
        }
    }

    fn run(
        self: Arc<Self>,
        input: serde_json::Value,
        _request: Arc<LanguageModelRequest>,
        _project: Entity<Project>,
        _action_log: Entity<ActionLog>,
        _model: Arc<dyn LanguageModel>,
        _window: Option<AnyWindowHandle>,
        cx: &mut App,
    ) -> ToolResult {
        // Parse discriminated schema (operation.type) and map to internal enum.
        // New expected shape:
        // {
        //   "operation": {
        //     "type": "append" | "search" | "answer" | "similar" | "list" | "get" | "reembed"
        //               | "update_metadata" | "config_get" | "config_set",
        //     ... variant-specific fields ...
        //   }
        // }
                let op_raw = input.get("operation").cloned().unwrap_or_else(|| serde_json::json!({}));
                let op_type = op_raw.get("type").and_then(|v| v.as_str()).unwrap_or("");
                let build_string = |k: &str| op_raw.get(k).and_then(|v| v.as_str()).map(|s| s.to_string());
                let build_bool = |k: &str| op_raw.get(k).and_then(|v| v.as_bool());
                let build_usize = |k: &str| op_raw.get(k).and_then(|v| v.as_u64()).map(|u| u as usize);
                let build_f32 = |k: &str| op_raw.get(k).and_then(|v| v.as_f64()).map(|f| f as f32);
                let build_vec_string = |k: &str| {
                    op_raw.get(k).and_then(|v| {
                        v.as_array().map(|arr| {
                            arr.iter()
                                .filter_map(|e| e.as_str().map(|s| s.to_string()))
                                .collect::<Vec<String>>()
                        })
                    })
                };

                let operation = match op_type {
                    "append" => {
                        let role = build_string("role").and_then(|r| match r.as_str() {
                            "User" => Some(MessageRole::User),
                            "Assistant" => Some(MessageRole::Assistant),
                            _ => None,
                        });
                        match build_string("content") {
                            Some(content) => ChatHistoryOperation::Append {
                                chat_id: build_string("chat_id"),
                                project_id: build_string("project_id"),
                                title: build_string("title"),
                                role,
                                content,
                            },
                            None => {
                                return ToolResult {
                                    output: Task::ready(Err(anyhow!("append: 'content' required"))),
                                    card: None,
                                }
                            }
                        }
                    }
                    "search" => {
                        match build_string("query") {
                            Some(query) => ChatHistoryOperation::Search {
                                query,
                                project_id: build_string("project_id"),
                                chat_id: build_string("chat_id"),
                                top_k: build_usize("top_k"),
                                mode: build_string("mode").and_then(|m| match m.as_str() {
                                    "bm25" => Some(RetrievalMode::Bm25),
                                    "embedding" => Some(RetrievalMode::Embedding),
                                    "hybrid" => Some(RetrievalMode::Hybrid),
                                    _ => None,
                                }),
                                alpha: build_f32("alpha"),
                            },
                            None => {
                                return ToolResult {
                                    output: Task::ready(Err(anyhow!("search: 'query' required"))),
                                    card: None,
                                }
                            }
                        }
                    }
                    "answer" => {
                        match build_string("question") {
                            Some(question) => ChatHistoryOperation::Answer {
                                question,
                                project_id: build_string("project_id"),
                                chat_id: build_string("chat_id"),
                                top_k: build_usize("top_k"),
                                mode: build_string("mode").and_then(|m| match m.as_str() {
                                    "bm25" => Some(RetrievalMode::Bm25),
                                    "embedding" => Some(RetrievalMode::Embedding),
                                    "hybrid" => Some(RetrievalMode::Hybrid),
                                    _ => None,
                                }),
                                alpha: build_f32("alpha"),
                            },
                            None => {
                                return ToolResult {
                                    output: Task::ready(Err(anyhow!("answer: 'question' required"))),
                                    card: None,
                                }
                            }
                        }
                    }
                    "similar" => {
                        match build_string("chat_id") {
                            Some(chat_id) => ChatHistoryOperation::Similar {
                                chat_id,
                                n: build_usize("n"),
                                project_scoped: build_bool("project_scoped"),
                            },
                            None => {
                                return ToolResult {
                                    output: Task::ready(Err(anyhow!("similar: 'chat_id' required"))),
                                    card: None,
                                }
                            }
                        }
                    }
                    "list" => ChatHistoryOperation::List {
                        project_id: build_string("project_id"),
                        limit: build_usize("limit"),
                        offset: build_usize("offset"),
                    },
                    "get" => {
                        match build_string("chat_id") {
                            Some(chat_id) => ChatHistoryOperation::Get { chat_id },
                            None => {
                                return ToolResult {
                                    output: Task::ready(Err(anyhow!("get: 'chat_id' required"))),
                                    card: None,
                                }
                            }
                        }
                    }
                    "reembed" => ChatHistoryOperation::Reembed {
                        chat_id: build_string("chat_id"),
                    },
                    "update_metadata" => {
                        match build_string("chat_id") {
                            Some(chat_id) => ChatHistoryOperation::UpdateMetadata {
                                chat_id,
                                title: build_string("title"),
                                summary: build_string("summary"),
                                tags_add: build_vec_string("tags_add"),
                                tags_remove: build_vec_string("tags_remove"),
                                archived: build_bool("archived"),
                                pinned: build_bool("pinned"),
                            },
                            None => {
                                return ToolResult {
                                    output: Task::ready(Err(anyhow!("update_metadata: 'chat_id' required"))),
                                    card: None,
                                }
                            }
                        }
                    }
                    "config_get" => ChatHistoryOperation::ConfigGet,
                    "config_set" => ChatHistoryOperation::ConfigSet {
                        embedding_model: build_string("embedding_model"),
                        hybrid_alpha: build_f32("hybrid_alpha"),
                        similar_chats_k: build_usize("similar_chats_k"),
                        summary_refresh_chars: build_usize("summary_refresh_chars"),
                        summary_delta_chars: build_usize("summary_delta_chars"),
                        rag_top_k: build_usize("rag_top_k"),
                        auto_tag: build_bool("auto_tag"),
                        default_retrieval_mode: build_string("default_retrieval_mode").and_then(|m| match m.as_str() {
                            "bm25" => Some(RetrievalMode::Bm25),
                            "embedding" => Some(RetrievalMode::Embedding),
                            "hybrid" => Some(RetrievalMode::Hybrid),
                            _ => None,
                        }),
                    },
                    other => {
                        return ToolResult {
                            output: Task::ready(Err(anyhow!(format!(
                                "Unknown operation.type '{}'",
                                other
                            )))),
                            card: None,
                        }
                    }
                };

                let parsed = ChatHistoryToolInput { operation };

        // Acquire adapter (fail fast if missing).
        let adapter = match adapter() {
            Ok(a) => a,
            Err(e) => {
                return ToolResult {
                    output: Task::ready(Err(e)),
                    card: None,
                }
            }
        };

        // Build async task performing operation.
        let task = cx.spawn(async move |_cx| {
            let resp_json_str = match parsed.operation {
                ChatHistoryOperation::Append {
                    chat_id,
                    project_id,
                    title,
                    role,
                    content,
                } => {
                    let payload = json!({
                        "chat_id": chat_id,
                        "project_id": project_id,
                        "title": title,
                        "role": role,
                        "content": content
                    })
                    .to_string();
                    adapter.chat_append(&payload).await
                }
                ChatHistoryOperation::Search {
                    query,
                    project_id,
                    chat_id,
                    top_k,
                    mode,
                    alpha,
                } => {
                    let payload = json!({
                        "query": query,
                        "project_id": project_id,
                        "chat_id": chat_id,
                        "top_k": top_k,
                        "mode": mode,
                        "alpha": alpha
                    })
                    .to_string();
                    adapter.chat_search(&payload).await
                }
                ChatHistoryOperation::Answer {
                    question,
                    project_id,
                    chat_id,
                    top_k,
                    mode,
                    alpha,
                } => {
                    let payload = json!({
                        "question": question,
                        "project_id": project_id,
                        "chat_id": chat_id,
                        "top_k": top_k,
                        "mode": mode,
                        "alpha": alpha
                    })
                    .to_string();
                    adapter.chat_answer(&payload).await
                }
                ChatHistoryOperation::Similar {
                    chat_id,
                    n,
                    project_scoped,
                } => {
                    let payload = json!({
                        "chat_id": chat_id,
                        "n": n,
                        "project_scoped": project_scoped
                    })
                    .to_string();
                    adapter.chat_similar(&payload).await
                }
                ChatHistoryOperation::List {
                    project_id,
                    limit,
                    offset,
                } => {
                    let payload = json!({
                        "project_id": project_id,
                        "limit": limit,
                        "offset": offset
                    })
                    .to_string();
                    adapter.chat_list(&payload).await
                }
                ChatHistoryOperation::Get { chat_id } => {
                    let payload = json!({ "chat_id": chat_id }).to_string();
                    adapter.chat_get(&payload).await
                }
                ChatHistoryOperation::Reembed { chat_id } => {
                    let payload = json!({ "chat_id": chat_id }).to_string();
                    adapter.chat_reembed(&payload).await
                }
                ChatHistoryOperation::UpdateMetadata {
                    chat_id,
                    title,
                    summary,
                    tags_add,
                    tags_remove,
                    archived,
                    pinned,
                } => {
                    let payload = json!({
                        "chat_id": chat_id,
                        "title": title,
                        "summary": summary,
                        "tags_add": tags_add,
                        "tags_remove": tags_remove,
                        "archived": archived,
                        "pinned": pinned
                    })
                    .to_string();
                    adapter.chat_update_metadata(&payload).await
                }
                ChatHistoryOperation::ConfigGet => adapter.chat_config_get().await,
                ChatHistoryOperation::ConfigSet {
                    embedding_model,
                    hybrid_alpha,
                    similar_chats_k,
                    summary_refresh_chars,
                    summary_delta_chars,
                    rag_top_k,
                    auto_tag,
                    default_retrieval_mode,
                } => {
                    let payload = json!({
                        "embedding_model": embedding_model,
                        "hybrid_alpha": hybrid_alpha,
                        "similar_chats_k": similar_chats_k,
                        "summary_refresh_chars": summary_refresh_chars,
                        "summary_delta_chars": summary_delta_chars,
                        "rag_top_k": rag_top_k,
                        "auto_tag": auto_tag,
                        "default_retrieval_mode": default_retrieval_mode
                    })
                    .to_string();
                    adapter.chat_config_set(&payload).await
                }
            };

            // Parse adapter response; treat ok:false as an error.
            let value: Value = serde_json::from_str(&resp_json_str)
                .map_err(|e| anyhow!("response parse error: {e}; raw={resp_json_str}"))?;
            if value
                .get("ok")
                .and_then(|v| v.as_bool())
                .is_some_and(|ok| !ok)
            {
                if let Some(err) = value.get("error").and_then(|e| e.as_str()) {
                    let owned_err = err.to_string();
                    return Err(anyhow!(owned_err));
                }
            }

            // Produce markdown wrapper.
            let mut md = String::new();
            md.push_str("# Chat History Tool Result\n\n```json\n");
            md.push_str(&serde_json::to_string_pretty(&value)?);
            md.push_str("\n```\n");
            Ok(ToolResultOutput::from(md))
        });

        ToolResult {
            output: task,
            card: None,
        }
    }
}
