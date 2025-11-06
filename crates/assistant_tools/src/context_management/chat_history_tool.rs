use std::sync::{Arc, OnceLock};

#[cfg(test)]
mod adapter_schema_tests {
    use super::*;
    use assistant_tool::Tool;
    use gpui::App;

    #[gpui::test]
    fn chat_history_tool_schema_contains_operation(cx: &mut App) {
        let tool = ChatHistoryTool;
        let schema = tool
            .input_schema(language_model::LanguageModelToolSchemaFormat::JsonSchemaSubset)
            .expect("schema");
        assert!(schema.get("properties").is_some(), "schema missing properties object");
        let op = &schema["properties"]["operation"];
        assert!(op.is_object(), "operation property should be an object");
    }

    #[gpui::test]
    fn chat_history_adapter_initially_none(cx: &mut App) {
        // Adapter should not be installed until startup code calls install_chat_history_adapter.
        assert!(chat_history_adapter().is_none(), "adapter unexpectedly installed at test start");
    }

    #[gpui::test]
    fn chat_history_adapter_install_and_retrieve(cx: &mut App) {
        use chat_history_tools::init::init_chat_history_tools;
        let handles = init_chat_history_tools(Default::default()).expect("init handles");
        install_chat_history_adapter(&handles);
        assert!(chat_history_adapter().is_some(), "adapter should be installed");
    }
}

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
    let result = CHAT_HISTORY_ADAPTER.set(handles.tools.clone());
    if result.is_ok() {
        log::info!("chat_history: adapter successfully installed and ready");
    } else {
        log::warn!("chat_history: adapter already installed, ignoring duplicate install attempt");
    }
}

/// Public getter for the installed chat history adapter.
/// Returns None if the adapter has not yet been installed.
pub fn chat_history_adapter() -> Option<Arc<ChatHistoryTools>> {
    CHAT_HISTORY_ADAPTER.get().cloned()
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
/// Chat history operations that can be performed.
///
/// # Serialization Format
///
/// This enum uses serde's default tagged representation with `#[serde(rename_all = "snake_case")]`.
///
/// ## Examples of correct JSON format:
///
/// ```json
/// // List chats (all fields optional)
/// {"list": {"limit": 10, "offset": 0}}
/// {"list": {}}  // Use defaults
///
/// // Find similar chats to current conversation (chat_id optional)
/// {"similar": {"n": 10, "project_scoped": true}}
/// {"similar": {"chat_id": "abc123", "n": 5}}
///
/// // Search messages
/// {"search": {"query": "rust async", "mode": "hybrid", "top_k": 10}}
///
/// // Answer question with RAG
/// {"answer": {"question": "how did I solve this before?"}}
///
/// // Get config (unit variant)
/// {"config_get": {}}
///
/// // Append message
/// {"append": {"content": "Hello", "role": "User"}}
/// ```
///
/// Each variant maps directly to a JSON method on `ChatHistoryTools`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
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
    /// Find similar chats to the given chat id. If chat_id is omitted, uses current conversation.
    Similar {
        #[serde(default)]
        chat_id: Option<String>,
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
    /// Create a new empty chat session.
    CreateChat {
        #[serde(default)]
        project_id: Option<String>,
        #[serde(default)]
        title: Option<String>,
    },
    /// Delete a chat and all its messages.
    DeleteChat {
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
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
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
        r#"Persistent chat history with semantic search, RAG, and similarity matching.

OPERATIONS:
• similar - Find chats semantically similar to THIS chat or a specified chat_id (discovers related conversations)
• search - Keyword/semantic search across all chat messages (hybrid BM25 + embeddings)
• answer - RAG synthesis: retrieve relevant context and generate an answer with citations
• list - List stored chats with metadata (paginated)
• get - Retrieve full chat with all messages
• append - Add a message to a chat (auto-creates if needed)
• create_chat - Create a new empty chat session
• delete_chat - Delete a chat and all its messages
• update_metadata - Edit title, summary, tags, archived/pinned status
• reembed - Recompute embeddings (requires confirmation)
• config_get/config_set - View/modify configuration

USAGE - Correct JSON format (tagged union):
The 'operation' parameter expects a tagged union format. Each operation is an object with the operation name as the key.

Examples:
1. Find chats related to THIS conversation:
   {"similar": {"n": 10, "project_scoped": true}}

2. Find chats similar to a specific chat:
   {"similar": {"chat_id": "abc123", "n": 10}}

3. Search all chats:
   {"search": {"query": "rust async", "mode": "hybrid", "top_k": 10}}

4. Answer from history:
   {"answer": {"question": "how did I solve X before?", "project_id": "my-project"}}

5. List recent chats:
   {"list": {"limit": 20, "offset": 0}}

6. Get config:
   {"config_get": {}}

7. Append message:
   {"append": {"content": "Some text", "role": "User", "chat_id": "abc123"}}

PARAMETERS:
- chat_id: Optional for 'similar' (defaults to current conversation's thread_id), required for get/delete/update_metadata
- mode: "bm25" (keyword), "embedding" (semantic), "hybrid" (both, default)
- alpha: 0.0 (keyword only) to 1.0 (semantic only), default 0.55 for hybrid
- project_scoped: limit search to current project (default: true for 'similar')
- top_k/n: number of results to return

TIPS:
- Use 'similar' without chat_id to find conversations related to the current topic
- Use 'search' for keyword/semantic queries across all messages
- Use 'answer' when you want a synthesized response with citations
- All struct fields are optional unless marked as required (e.g., query, question, content)"#.into()
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
        // Comprehensive schema with detailed descriptions for each operation type.
        // Structured as a flat discriminated union for easier LLM consumption.
        let schema = json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "object",
                    "properties": {
                        "type": {
                            "type": "string",
                            "description": "Operation type. Use 'similar' to find related chats, 'search' for keyword/semantic search, 'answer' for RAG synthesis.",
                            "enum": [
                                "similar", "search", "answer", "list", "get", "append",
                                "create_chat", "delete_chat", "reembed", "update_metadata",
                                "config_get", "config_set"
                            ]
                        },

                        // Core identifiers
                        "chat_id": {
                            "type": "string",
                            "description": "Chat identifier. Required for: get, delete_chat, update_metadata. Optional for: similar (defaults to current chat), search (scope to one chat), append (auto-creates if missing)."
                        },
                        "project_id": {
                            "type": "string",
                            "description": "Project identifier. Optional for: list, search, answer, create_chat, append."
                        },

                        // Search/retrieval parameters
                        "query": {
                            "type": "string",
                            "description": "Search query text. Required for 'search' operation. Supports keyword and semantic matching."
                        },
                        "question": {
                            "type": "string",
                            "description": "Question for RAG synthesis. Required for 'answer' operation. Returns answer with citations from chat history."
                        },
                        "mode": {
                            "type": "string",
                            "enum": ["bm25", "embedding", "hybrid"],
                            "description": "Retrieval mode. 'bm25' = keyword only, 'embedding' = semantic only, 'hybrid' = both (default). Used by: search, answer."
                        },
                        "alpha": {
                            "type": "number",
                            "minimum": 0.0,
                            "maximum": 1.0,
                            "description": "Hybrid fusion weight. 0.0 = pure keyword, 1.0 = pure semantic, 0.55 = balanced (default). Only applies when mode='hybrid'."
                        },
                        "top_k": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "Number of results to return. Used by: search, answer. Default varies by operation."
                        },

                        // Similarity parameters
                        "n": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "Number of similar chats to return. Used by 'similar' operation. Default: 10."
                        },
                        "project_scoped": {
                            "type": "boolean",
                            "description": "Limit similarity search to current project. Used by 'similar' operation. Default: true."
                        },

                        // List/pagination
                        "limit": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "Maximum number of chats to return. Used by 'list' operation."
                        },
                        "offset": {
                            "type": "integer",
                            "minimum": 0,
                            "description": "Pagination offset for 'list' operation."
                        },

                        // Message content
                        "title": {
                            "type": "string",
                            "description": "Chat title. Used by: create_chat, append (sets title on creation), update_metadata."
                        },
                        "role": {
                            "type": "string",
                            "enum": ["User", "Assistant", "System", "Tool"],
                            "description": "Message role. Used by 'append' operation. Default: User."
                        },
                        "content": {
                            "type": "string",
                            "description": "Message content. Required for 'append' operation."
                        },

                        // Metadata updates
                        "summary": {
                            "type": "string",
                            "description": "Chat summary. Used by 'update_metadata' operation."
                        },
                        "tags_add": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Tags to add. Used by 'update_metadata' operation."
                        },
                        "tags_remove": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Tags to remove. Used by 'update_metadata' operation."
                        },
                        "archived": {
                            "type": "boolean",
                            "description": "Archive status. Used by 'update_metadata' operation."
                        },
                        "pinned": {
                            "type": "boolean",
                            "description": "Pin status. Used by 'update_metadata' operation."
                        },

                        // Configuration
                        "embedding_model": {
                            "type": "string",
                            "description": "Embedding model name. Used by 'config_set' operation."
                        },
                        "hybrid_alpha": {
                            "type": "number",
                            "description": "Default hybrid fusion weight. Used by 'config_set' operation."
                        },
                        "similar_chats_k": {
                            "type": "integer",
                            "description": "Default number of similar chats to return. Used by 'config_set' operation."
                        },
                        "summary_refresh_chars": {
                            "type": "integer",
                            "description": "Character threshold for summary refresh. Used by 'config_set' operation."
                        },
                        "summary_delta_chars": {
                            "type": "integer",
                            "description": "Delta for summary updates. Used by 'config_set' operation."
                        },
                        "rag_top_k": {
                            "type": "integer",
                            "description": "Default top-k for RAG retrieval. Used by 'config_set' operation."
                        },
                        "auto_tag": {
                            "type": "boolean",
                            "description": "Enable automatic tagging. Used by 'config_set' operation."
                        },
                        "default_retrieval_mode": {
                            "type": "string",
                            "enum": ["bm25", "embedding", "hybrid"],
                            "description": "Default retrieval mode. Used by 'config_set' operation."
                        }
                    },
                    "required": ["type"]
                }
            },
            "required": ["operation"],
            "examples": [
                {
                    "description": "Find chats similar to the current one (omit chat_id to use current conversation)",
                    "value": {
                        "operation": {
                            "type": "similar",
                            "n": 10,
                            "project_scoped": true
                        }
                    }
                },
                {
                    "description": "Find chats similar to a specific chat",
                    "value": {
                        "operation": {
                            "type": "similar",
                            "chat_id": "specific-chat-123",
                            "n": 10
                        }
                    }
                },
                {
                    "description": "Search for messages about async programming",
                    "value": {
                        "operation": {
                            "type": "search",
                            "query": "async await rust",
                            "mode": "hybrid",
                            "top_k": 10
                        }
                    }
                },
                {
                    "description": "Get answer from chat history with citations",
                    "value": {
                        "operation": {
                            "type": "answer",
                            "question": "How did I implement error handling in the last project?",
                            "project_id": "my-project"
                        }
                    }
                },
                {
                    "description": "List recent chats",
                    "value": {
                        "operation": {
                            "type": "list",
                            "limit": 20,
                            "offset": 0
                        }
                    }
                }
            ]
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
                    if let Some(id) = chat_id {
                        format!("Similar chats to {id}")
                    } else {
                        "Similar chats to current conversation".into()
                    }
                }
                ChatHistoryOperation::List { .. } => "List chats".into(),
                ChatHistoryOperation::Get { chat_id } => format!("Get chat {chat_id}"),
                ChatHistoryOperation::CreateChat { .. } => "Create new chat".into(),
                ChatHistoryOperation::DeleteChat { chat_id } => format!("Delete chat {chat_id}"),
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
        request: Arc<LanguageModelRequest>,
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
                        ChatHistoryOperation::Similar {
                            chat_id: build_string("chat_id"),
                            n: build_usize("n"),
                            project_scoped: build_bool("project_scoped"),
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
                    "create_chat" => ChatHistoryOperation::CreateChat {
                        project_id: build_string("project_id"),
                        title: build_string("title"),
                    },
                    "delete_chat" => {
                        match build_string("chat_id") {
                            Some(chat_id) => ChatHistoryOperation::DeleteChat { chat_id },
                            None => {
                                return ToolResult {
                                    output: Task::ready(Err(anyhow!("delete_chat: 'chat_id' required"))),
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
                    // Use current conversation's thread_id if chat_id not provided
                    let effective_chat_id = chat_id.or_else(|| request.thread_id.clone());
                    if effective_chat_id.is_none() {
                        return Err(anyhow!("similar: 'chat_id' required or current conversation must have thread_id"));
                    }
                    let payload = json!({
                        "chat_id": effective_chat_id,
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
                ChatHistoryOperation::CreateChat { project_id, title } => {
                    let payload = json!({
                        "project_id": project_id,
                        "title": title
                    })
                    .to_string();
                    adapter.chat_create(&payload).await
                }
                ChatHistoryOperation::DeleteChat { chat_id } => {
                    let payload = json!({ "chat_id": chat_id }).to_string();
                    adapter.chat_delete(&payload).await
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

            // Return raw JSON (no markdown wrapper)
            Ok(ToolResultOutput::from(serde_json::to_string_pretty(&value)?))
        });

        ToolResult {
            output: task,
            card: None,
        }
    }
}
