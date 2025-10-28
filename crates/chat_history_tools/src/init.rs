use std::sync::Arc;

use anyhow::{anyhow, Result};
use chat_history::{
    AzureOpenAIEmbeddingBackend, AzureOpenAiConfig, ChatHistoryConfig, ChatStore, EmbeddingBackend,
    EmbeddingBackendKind, FastEmbedBackend, OpenAIEmbeddingBackend, OpenAiConfig,
};
use tokio::sync::Mutex;

use crate::ChatHistoryTools;

/// Options controlling initialization of the chat history memory system.
///
/// This deliberately keeps DB integration optional. When a database-backed
/// `ChatHistoryDb` becomes available, an additional field can be added to
/// supply a connection / handle.
#[derive(Clone, Debug)]
pub struct ChatHistoryInitOptions {
    /// Override embedding backend (default: FastEmbed local).
    pub backend: Option<EmbeddingBackendKind>,
    /// Override embedding model identifier (default taken from `ChatHistoryConfig::default()`).
    pub embedding_model: Option<String>,
    /// Optional override for hybrid alpha.
    pub hybrid_alpha: Option<f32>,
    /// Optional override for similar chats K.
    pub similar_chats_k: Option<usize>,
    /// Optional summary refresh character threshold.
    pub summary_refresh_chars: Option<usize>,
    /// Optional summary delta threshold.
    pub summary_delta_chars: Option<usize>,
    /// Optional RAG top-k default.
    pub rag_top_k: Option<usize>,
    /// Enable / disable auto tag suggestion (default true).
    pub auto_tag: Option<bool>,
    /// OpenAI specific overrides.
    pub openai: Option<OpenAiConfig>,
    /// Azure OpenAI specific overrides.
    pub azure_openai: Option<AzureOpenAiConfig>,
}

impl Default for ChatHistoryInitOptions {
    fn default() -> Self {
        Self {
            backend: None,
            embedding_model: None,
            hybrid_alpha: None,
            similar_chats_k: None,
            summary_refresh_chars: None,
            summary_delta_chars: None,
            rag_top_k: None,
            auto_tag: None,
            openai: None,
            azure_openai: None,
        }
    }
}

/// Aggregated handles returned after successful initialization.
pub struct ChatHistoryHandles {
    /// Thread-safe store for direct programmatic access.
    pub store: Arc<Mutex<ChatStore>>,
    /// Tool adapter exposing snake_case JSON APIs.
    pub tools: Arc<ChatHistoryTools>,
}

/// Initialize the chat history subsystem and return both `ChatStore` and tool adapter.
///
/// This currently instantiates a local FastEmbed backend by default. When
/// OpenAI / Azure support is desired, supply `options.backend`.
///
/// Example:
/// ```ignore
/// let handles = init_chat_history_tools(Default::default())?;
/// let answer_json = handles.tools.chat_answer(r#"{"question":"What did we discuss?"}"#).await;
/// ```
pub fn init_chat_history_tools(options: ChatHistoryInitOptions) -> Result<ChatHistoryHandles> {
    // Load settings from .zed/settings.json (or settings.json) first, then layer explicit overrides.
    // Explicit ChatHistoryInitOptions fields below take precedence over file-based settings.
    let mut config = ChatHistoryConfig::load_from_default_files();

    if let Some(model) = &options.embedding_model {
        config.embedding_model = model.clone();
    }
    if let Some(alpha) = options.hybrid_alpha {
        config.hybrid_alpha = alpha;
    }
    if let Some(k) = options.similar_chats_k {
        config.similar_chats_k = k;
    }
    if let Some(v) = options.summary_refresh_chars {
        config.summary_refresh_chars = v;
    }
    if let Some(v) = options.summary_delta_chars {
        config.summary_delta_chars = v;
    }
    if let Some(v) = options.rag_top_k {
        config.rag_top_k = v;
    }
    if let Some(v) = options.auto_tag {
        config.auto_tag = v;
    }
    if let Some(ref o) = options.openai {
        // Layer OpenAI overrides
        if let Some(model) = &o.model {
            config.openai.model = Some(model.clone());
        }
        if let Some(key) = &o.api_key {
            config.openai.api_key = Some(key.clone());
        }
        if let Some(url) = &o.api_url {
            config.openai.api_url = Some(url.clone());
        }
    }
    if let Some(ref az) = options.azure_openai {
        if let Some(key) = &az.api_key {
            config.azure_openai.api_key = Some(key.clone());
        }
        if let Some(ep) = &az.endpoint {
            config.azure_openai.endpoint = Some(ep.clone());
        }
        if let Some(ver) = &az.api_version {
            config.azure_openai.api_version = Some(ver.clone());
        }
        if let Some(dep) = &az.deployment {
            config.azure_openai.deployment = Some(dep.clone());
        }
        if let Some(model) = &az.embedding_model {
            config.azure_openai.embedding_model = Some(model.clone());
        }
    }

    let backend_kind = options
        .backend
        .clone()
        .unwrap_or(EmbeddingBackendKind::FastEmbedLocal { model_path: None });

    // Select and construct embedding backend implementation.
    let backend: Arc<dyn EmbeddingBackend> = match backend_kind {
        EmbeddingBackendKind::FastEmbedLocal { model_path: _ } => Arc::new(FastEmbedBackend::new(
            config
                .openai
                .model
                .clone()
                .or_else(|| config.azure_openai.embedding_model.clone())
                .unwrap_or(config.embedding_model.clone()),
        )),
        EmbeddingBackendKind::OpenAI { .. } => {
            // OpenAI backend currently only requires a model name; API key/url validated for presence.
            let _api_key = config
                .openai
                .api_key
                .clone()
                .ok_or_else(|| anyhow!("openai.api_key not set"))?;
            let _ = config
                .openai
                .api_url
                .clone()
                .unwrap_or_else(|| "https://api.openai.com/v1".into());
            Arc::new(OpenAIEmbeddingBackend::new(
                config
                    .openai
                    .model
                    .clone()
                    .unwrap_or_else(|| "text-embedding-3-small".into()),
            ))
        }
        EmbeddingBackendKind::AzureOpenAI { .. } => {
            let api_key = config
                .azure_openai
                .api_key
                .clone()
                .ok_or_else(|| anyhow!("azure_openai.api_key not set"))?;
            let endpoint = config
                .azure_openai
                .endpoint
                .clone()
                .ok_or_else(|| anyhow!("azure_openai.endpoint not set"))?;
            let api_version = config
                .azure_openai
                .api_version
                .clone()
                .unwrap_or_else(|| "2024-02-15-preview".into());
            let deployment = config
                .azure_openai
                .deployment
                .clone()
                .ok_or_else(|| anyhow!("azure_openai.deployment not set"))?;
            // AzureOpenAIEmbeddingBackend currently a stub; provide graceful error until implemented.
            Arc::new(AzureOpenAIEmbeddingBackend::new_with_config(
                config
                    .azure_openai
                    .embedding_model
                    .clone()
                    .unwrap_or_else(|| deployment.clone()),
                endpoint,
                api_key,
                api_version,
                deployment,
            ))
        }
    };

    // For now we do not attach a DB (None). Future: supply ChatHistoryDb here.
    let store = ChatStore::new(backend, config, None);
    let store_arc = Arc::new(Mutex::new(store));

    let tools = Arc::new(ChatHistoryTools::new(store_arc.clone()));

    Ok(ChatHistoryHandles {
        store: store_arc,
        tools,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{env, fs, io::Write, path::PathBuf};

    fn write_settings(dir: &PathBuf, json: &str) {
        let zed_dir = dir.join(".zed");
        fs::create_dir_all(&zed_dir).unwrap();
        let mut f = fs::File::create(zed_dir.join("settings.json")).unwrap();
        f.write_all(json.as_bytes()).unwrap();
    }

    #[test]
    fn precedence_file_then_options() {
        // Prepare isolated directory
        let original_dir = env::current_dir().unwrap();
        let test_dir = original_dir.join("chat_history_tools_precedence_test");
        if test_dir.exists() {
            fs::remove_dir_all(&test_dir).unwrap();
        }
        fs::create_dir_all(&test_dir).unwrap();

        // Write settings file with base values
        write_settings(
            &test_dir,
            r#"
{
  "chat_history": {
    "embedding_model": "from-settings",
    "rag_top_k": 3
  }
}
"#,
        );

        // Change into test directory so loader finds our settings
        env::set_current_dir(&test_dir).unwrap();

        // Provide overrides via options
        let handles = init_chat_history_tools(ChatHistoryInitOptions {
            embedding_model: Some("override-model".into()),
            rag_top_k: Some(9),
            ..Default::default()
        })
        .expect("init should succeed");

        // Inspect effective config (async lock)
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (embedding_model, rag_top_k) = rt.block_on(async {
            let store = handles.store.lock().await;
            (
                store.config().embedding_model.clone(),
                store.config().rag_top_k,
            )
        });

        // Restore working directory
        env::set_current_dir(&original_dir).unwrap();

        // Assertions: options should override file
        assert_eq!(embedding_model, "override-model");
        assert_eq!(rag_top_k, 9);

        // Clean up
        if test_dir.exists() {
            fs::remove_dir_all(&test_dir).unwrap();
        }
    }
}
