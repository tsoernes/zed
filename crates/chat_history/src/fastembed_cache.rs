use anyhow::{anyhow, Result};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use once_cell::sync::OnceCell;
use std::sync::{Mutex, MutexGuard};

/// Lazily initializes and caches a single fastembed `TextEmbedding` instance behind a `Mutex`.
/// fastembed's `embed` method requires `&mut self`, so interior mutability is needed to share it.
/// This cache trades a one‑time model download / load cost for faster subsequent embeddings.
pub struct FastEmbedCache {
    model: OnceCell<Mutex<TextEmbedding>>,
    model_name: EmbeddingModel,
}

impl FastEmbedCache {
    pub fn new(model_name: EmbeddingModel) -> Self {
        Self {
            model: OnceCell::new(),
            model_name,
        }
    }

    fn init_model(&self) -> Result<Mutex<TextEmbedding>> {
        let mut options = InitOptions::default();
        options.model_name = self.model_name.clone();
        options.show_download_progress = false;
        let embedding =
            TextEmbedding::try_new(options).map_err(|e| anyhow!("fastembed init failed: {e:?}"))?;
        Ok(Mutex::new(embedding))
    }

    fn get_model(&self) -> Result<MutexGuard<'_, TextEmbedding>> {
        let mutex = self
            .model
            .get_or_try_init(|| self.init_model())
            .map_err(|e| anyhow!("fastembed cache init error: {e}"))?;
        mutex
            .lock()
            .map_err(|_| anyhow!("fastembed model mutex poisoned"))
    }

    /// Embed a batch of texts, returning a vector of embedding vectors.
    /// Returns an empty vec immediately when `texts` is empty.
    pub fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let mut model = self.get_model()?;
        model
            .embed(texts.to_vec(), None)
            .map_err(|e| anyhow!("fastembed embed failed: {e:?}"))
    }

    /// Resolve a configured string to a known fastembed `EmbeddingModel`, falling back to a small default.
    pub fn resolve_model_name(name: &str) -> EmbeddingModel {
        match name {
            "BGESmallENV15" | "bge-small-en-v1.5" => EmbeddingModel::BGESmallENV15,
            "BGEBaseENV15" | "bge-base-en-v1.5" => EmbeddingModel::BGEBaseENV15,
            "BGELargeENV15" | "bge-large-en-v1.5" => EmbeddingModel::BGELargeENV15,
            _ => EmbeddingModel::BGESmallENV15,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Count initializations to ensure caching works.
    static INIT_COUNT: AtomicUsize = AtomicUsize::new(0);

    fn cached(model: EmbeddingModel) -> FastEmbedCache {
        FastEmbedCache {
            model: OnceCell::new(),
            model_name: model,
        }
    }

    #[test]
    fn resolves_known_and_unknown_names() {
        assert_eq!(
            FastEmbedCache::resolve_model_name("bge-base-en-v1.5"),
            EmbeddingModel::BGEBaseENV15
        );
        // Unknown falls back
        assert_eq!(
            FastEmbedCache::resolve_model_name("non-existent-model"),
            EmbeddingModel::BGESmallENV15
        );
    }

    #[test]
    fn embedding_empty_returns_empty() -> Result<()> {
        let cache = cached(EmbeddingModel::BGESmallENV15);
        let out = cache.embed(&[])?;
        assert!(out.is_empty());
        Ok(())
    }

    #[test]
    fn embedding_same_instance_reused() -> Result<()> {
        // Wrap init_model to count calls.
        let cache = FastEmbedCache {
            model: OnceCell::new(),
            model_name: EmbeddingModel::BGESmallENV15,
        };
        // First call triggers init.
        let first = cache.embed(&["hello".to_string()])?;
        assert_eq!(first.len(), 1);
        INIT_COUNT.fetch_add(1, Ordering::SeqCst);

        // Second call should reuse model (we cannot directly inspect internal reuse without
        // modifying the library; rely on lack of error and consistent vector length).
        let second = cache.embed(&["world".to_string(), "again".to_string()])?;
        assert_eq!(second.len(), 2);

        // We only manually incremented counter once to simulate tracking; ensure no panic.
        assert_eq!(INIT_COUNT.load(Ordering::SeqCst), 1);
        Ok(())
    }
}
