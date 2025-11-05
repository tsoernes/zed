use std::sync::Arc;

use anyhow::{anyhow, Result};
use gpui::{App, Global};
use serde::{Deserialize, Serialize};

/// Metadata describing a stored memory segment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySegmentMeta {
    pub id: u64,
    pub start: usize,
    pub end: usize,
    pub count: usize,
    pub chars: usize,
    pub placeholder_chars: usize,
    pub token_savings_estimate: usize,
    pub summary: String,
    pub stored_epoch_ms: u128,
}

/// Detailed stored memory segment including (optionally) original messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySegmentDetail {
    pub meta: MemorySegmentMeta,
    pub messages: Vec<String>,
}

/// Aggregate statistics across all stored segments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryStats {
    pub segments: usize,
    pub messages: usize,
    pub chars: usize,
}

/// Thread (or other source) backed memory operations.
///
/// Implementations mutate / read application state via the single‐threaded
/// foreground `App` reference passed in, avoiding interior mutability inside
/// the backend itself.
pub trait MemoryBackend: Send + Sync {
    fn list(&self, app: &App, limit: Option<usize>) -> Result<Vec<MemorySegmentMeta>>;
    fn stats(&self, app: &App) -> Result<MemoryStats>;
    fn store(
        &self,
        app: &mut App,
        start: usize,
        end_exclusive: usize,
        summary: Option<String>,
    ) -> Result<MemorySegmentMeta>;
    fn load(&self, app: &App, id: u64, include_messages: bool) -> Result<MemorySegmentDetail>;
    fn restore(&self, app: &mut App, id: u64) -> Result<MemorySegmentDetail>;
}

/// Noop backend used until a real (thread-backed) implementation is installed.
///
/// Returns empty results for read operations and explicit errors for mutations
/// so callers can surface actionable feedback.
struct NoopMemoryBackend;

impl MemoryBackend for NoopMemoryBackend {
    fn list(&self, _app: &App, _limit: Option<usize>) -> Result<Vec<MemorySegmentMeta>> {
        Ok(Vec::new())
    }

    fn stats(&self, _app: &App) -> Result<MemoryStats> {
        Ok(MemoryStats {
            segments: 0,
            messages: 0,
            chars: 0,
        })
    }

    fn store(
        &self,
        _app: &mut App,
        _start: usize,
        _end_exclusive: usize,
        _summary: Option<String>,
    ) -> Result<MemorySegmentMeta> {
        Err(anyhow!("memory backend not installed"))
    }

    fn load(&self, _app: &App, _id: u64, _include_messages: bool) -> Result<MemorySegmentDetail> {
        Err(anyhow!("memory backend not installed"))
    }

    fn restore(&self, _app: &mut App, _id: u64) -> Result<MemorySegmentDetail> {
        Err(anyhow!("memory backend not installed"))
    }

    // Prune operation removed (memory segments are retained; explicit deletion is disabled).
}

/// Global wrapper storing the active `MemoryBackend` implementation.
struct GlobalMemoryBackendInner(Arc<dyn MemoryBackend>);

impl Global for GlobalMemoryBackendInner {}

impl Default for GlobalMemoryBackendInner {
    fn default() -> Self {
        GlobalMemoryBackendInner(Arc::new(NoopMemoryBackend))
    }
}

/// Public API for accessing / installing the global memory backend.
pub struct GlobalMemoryBackend;

impl GlobalMemoryBackend {
    /// Obtain the currently registered backend (defaults to noop).
    pub fn get(cx: &mut App) -> Arc<dyn MemoryBackend> {
        // If not installed yet, create the default (noop) instance.
        cx.default_global::<GlobalMemoryBackendInner>().0.clone()
    }
}
