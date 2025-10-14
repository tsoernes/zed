use std::sync::Arc;

use anyhow::{Result, anyhow};
use gpui::App;

use assistant_tools::context_management::{
    GlobalMemoryBackend, MemoryBackend, MemorySegmentDetail, MemorySegmentMeta, MemoryStats,
};

use crate::embedded_mcp_server::GlobalActiveThread;
use crate::thread::Thread;

/// ThreadMemoryBackend bridges the assistant memory tool to the active `agent2::Thread`.
/// It translates MemoryBackend trait calls into thread entity updates / reads.
/// Errors are surfaced when no active thread is available or when indices are invalid.
pub struct ThreadMemoryBackend;

impl ThreadMemoryBackend {
    /// Retrieve the active thread entity or return an error.
    fn active_thread(app: &App) -> Result<gpui::Entity<Thread>> {
        GlobalActiveThread::active_thread(app).ok_or_else(|| anyhow!("No active thread available"))
    }

    /// Convert a tuple returned by `Thread::memory_segment_metas()` into `MemorySegmentMeta`.
    fn tuple_to_meta(
        t: (u64, usize, usize, usize, usize, usize, usize, String, u128),
    ) -> MemorySegmentMeta {
        MemorySegmentMeta {
            id: t.0,
            start: t.1,
            end: t.2,
            count: t.3,
            chars: t.4,
            placeholder_chars: t.5,
            token_savings_estimate: t.6,
            summary: t.7,
            stored_epoch_ms: t.8,
        }
    }

    /// Parse the JSON metadata produced by `Thread::load_memory_segment`.
    fn json_to_meta(meta: &serde_json::Value) -> Result<MemorySegmentMeta> {
        Ok(MemorySegmentMeta {
            id: meta
                .get("id")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| anyhow!("missing id"))?,
            start: meta
                .get("start")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| anyhow!("missing start"))? as usize,
            end: meta
                .get("end")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| anyhow!("missing end"))? as usize,
            count: meta
                .get("count")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| anyhow!("missing count"))? as usize,
            chars: meta
                .get("chars")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| anyhow!("missing chars"))? as usize,
            placeholder_chars: meta
                .get("placeholder_chars")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| anyhow!("missing placeholder_chars"))?
                as usize,
            token_savings_estimate: meta
                .get("token_savings_estimate")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize,
            summary: meta
                .get("summary")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            stored_epoch_ms: meta
                .get("stored_epoch_ms")
                .and_then(|v| v.as_u64())
                .map(|v| v as u128)
                .unwrap_or_default(),
        })
    }
}

impl MemoryBackend for ThreadMemoryBackend {
    fn list(&self, app: &App, limit: Option<usize>) -> Result<Vec<MemorySegmentMeta>> {
        let thread = Self::active_thread(app)?;
        let metas = thread.read(app).memory_segment_metas();
        let trimmed = if let Some(l) = limit {
            if metas.len() > l {
                metas
                    .into_iter()
                    .rev()
                    .take(l)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect()
            } else {
                metas
            }
        } else {
            metas
        };
        Ok(trimmed.into_iter().map(Self::tuple_to_meta).collect())
    }

    fn stats(&self, app: &App) -> Result<MemoryStats> {
        let thread = Self::active_thread(app)?;
        let metas = thread.read(app).memory_segment_metas();
        let messages = metas.iter().map(|m| m.3).sum::<usize>();
        let chars = metas.iter().map(|m| m.4).sum::<usize>();
        Ok(MemoryStats {
            segments: metas.len(),
            messages,
            chars,
        })
    }

    fn store(
        &self,
        app: &mut App,
        start: usize,
        end_exclusive: usize,
        summary: Option<String>,
    ) -> Result<MemorySegmentMeta> {
        if start >= end_exclusive {
            return Err(anyhow!("start must be < end_exclusive"));
        }
        let thread = Self::active_thread(app)?;
        let inclusive_end = end_exclusive - 1;
        // Perform mutation via thread.update to archive the segment (pass optional custom summary).
        let id = thread.update(app, |thread, cx| {
            thread.store_memory_segment_with_summary(start, inclusive_end, summary.as_deref(), cx)
        })?;
        // Fetch metadata and return the stored segment.
        let metas = thread.read(app).memory_segment_metas();
        let meta_tuple = metas
            .into_iter()
            .find(|m| m.0 == id)
            .ok_or_else(|| anyhow!("stored segment not found"))?;
        Ok(Self::tuple_to_meta(meta_tuple))
    }

    fn load(&self, app: &App, id: u64, include_messages: bool) -> Result<MemorySegmentDetail> {
        let thread = Self::active_thread(app)?;
        let (meta_json, messages) = thread.read(app).load_memory_segment(id)?;
        let meta = Self::json_to_meta(&meta_json)?;
        Ok(MemorySegmentDetail {
            meta,
            messages: if include_messages {
                messages
            } else {
                Vec::new()
            },
        })
    }

    fn restore(&self, app: &mut App, id: u64) -> Result<MemorySegmentDetail> {
        let thread = Self::active_thread(app)?;
        thread.update(app, |thread, cx| thread.restore_memory_segment(id, cx))?;
        let (meta_json, messages) = thread.read(app).load_memory_segment(id)?;
        let meta = Self::json_to_meta(&meta_json)?;
        Ok(MemorySegmentDetail { meta, messages })
    }

    fn prune(&self, app: &mut App, id: u64) -> Result<()> {
        let thread = Self::active_thread(app)?;
        thread.update(app, |thread, cx| thread.prune_memory_segment(id, cx))?;
        Ok(())
    }
}

/// Install the thread-backed memory backend, replacing any existing backend.
/// Call this after `GlobalActiveThread` is initialized (e.g., in `init_agent2`).
pub fn install_thread_memory_backend(app: &mut App) {
    GlobalMemoryBackend::set_backend(app, Arc::new(ThreadMemoryBackend));
    log::info!("agent2::thread_memory_backend installed ThreadMemoryBackend");
}
