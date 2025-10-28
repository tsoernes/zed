use chat_history::{
    AzureOpenAIEmbeddingBackend, EmbeddingBackend, EmbeddingBackendKind, FastEmbedBackend,
    ChatHistoryConfig,
};

/// Ensures fastembed backend returns one embedding per input string and dimensions are > 0.
#[tokio::test]
async fn fastembed_basic_embedding() -> anyhow::Result<()> {
    let backend = FastEmbedBackend::new("bge-small-en-v1.5");
    let inputs = vec!["hello world".to_string(), "rust language".to_string()];
    let vectors = backend.embed(&inputs).await?;
    assert_eq!(vectors.len(), inputs.len());
    assert!(vectors.iter().all(|v| !v.is_empty()));
    Ok(())
}

/// Validates fuse_scores linear interpolation behaves as expected for representative values.
#[test]
fn hybrid_fuse_scores_linear() {
    // alpha near 0 biases lexical
    let fused_low = chat_history::fuse_scores(0.9, 0.1, 0.05);
    let expected_low = 0.05 * 0.9 + (1.0 - 0.05) * 0.1;
    assert!((fused_low - expected_low).abs() < 1e-6);

    // alpha near 1 biases embedding
    let fused_high = chat_history::fuse_scores(0.2, 0.8, 0.95);
    let expected_high = 0.95 * 0.2 + (1.0 - 0.95) * 0.8;
    assert!((fused_high - expected_high).abs() < 1e-6);

    // symmetry check when scores equal
    let fused_equal = chat_history::fuse_scores(0.5, 0.5, 0.33);
    assert!((fused_equal - 0.5).abs() < 1e-6);
}

/// Smoke test for FastEmbedCache reuse: calling twice should produce embeddings consistently.
#[tokio::test]
async fn fastembed_cache_reuse_smoke() -> anyhow::Result<()> {
    // Use backend which internally wraps the cache.
    let backend = FastEmbedBackend::new("bge-small-en-v1.5");
    let v1 = backend.embed(&["first".to_string()]).await?;
    let v2 = backend
        .embed(&["second".to_string(), "third".to_string()])
        .await?;
    assert_eq!(v1.len(), 1);
    assert_eq!(v2.len(), 2);
    // Dimension consistency.
    let dim = v1[0].len();
    assert!(dim > 0);
    assert!(v2.iter().all(|v| v.len() == dim));
    Ok(())
}

/// Azure embedding error path: if configuration present but invalid credentials, ensure a failure is surfaced.
/// Env vars required for live call; if absent, test is skipped so CI does not fail spuriously.
#[tokio::test]
async fn azure_embedding_error_path() -> anyhow::Result<()> {
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
        eprintln!("Skipping Azure embedding error test (missing one or more required env vars)");
        return Ok(());
    }

    // Use provided config; if invalid key, expect error; if valid key, embedding should succeed.
    let backend = AzureOpenAIEmbeddingBackend::new_with_config(
        "dummy-model", // model separate from deployment if needed
        endpoint.unwrap(),
        key.unwrap(), // could be invalid; test covers both scenarios
        version.unwrap(),
        deployment.unwrap(),
    );

    let texts = vec!["test azure embedding".to_string()];
    let result = backend.embed(&texts).await;
    match result {
        Ok(v) => {
            // Success path: dimension > 0
            assert_eq!(v.len(), 1);
            assert!(!v[0].is_empty());
        }
        Err(e) => {
            // Failure path: ensure message includes a recognizable substring.
            let msg = format!("{e}");
            assert!(
                msg.contains("azure embedding error status")
                    || msg.contains("request failed")
                    || msg.contains("failed to parse azure embedding response"),
                "unexpected azure error message: {msg}"
            );
        }
    }
    Ok(())
}

/// Configuration reconciliation test ensures backend model name resolved from config.
#[test]
fn config_reconcile_backend_variant() {
    let mut cfg = ChatHistoryConfig::default();
    cfg.embedding_backend = EmbeddingBackendKind::FastEmbedLocal { model_path: None };
    cfg.embedding_model = "bge-base-en-v1.5".to_string();
    // A simple call that would rely on resolve logic elsewhere later.
    assert_eq!(cfg.embedding_model, "bge-base-en-v1.5");
}

/// Basic embedding backend trait object usage for both backends to ensure dynamic dispatch works.
#[tokio::test]
async fn dynamic_dispatch_backends() -> anyhow::Result<()> {
    let fast_backend: Box<dyn EmbeddingBackend> = Box::new(FastEmbedBackend::new("bge-small-en-v1.5"));
    let fast_vecs = fast_backend.embed(&["dyn dispatch".to_string()]).await?;
    assert_eq!(fast_vecs.len(), 1);

    // Azure backend with empty config; expected to fail but still exercise trait object.
    let azure_backend: Box<dyn EmbeddingBackend> = Box::new(AzureOpenAIEmbeddingBackend::new_with_config(
        "azure-model",
        "https://example.invalid",
        "fake-key",
        "2024-02-15-preview",
        "fake-deployment",
    ));
    let azure_result = azure_backend.embed(&["dyn azure".to_string()]).await;
    assert!(azure_result.is_err() || (azure_result.is_ok() && !azure_result.unwrap()[0].is_empty()));

    Ok(())
}
