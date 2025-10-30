use std::sync::Arc;

use anyhow::{Result, anyhow};
use futures::future::{BoxFuture, FutureExt, join_all};
use gpui::{App, AsyncApp};
use language_model::{
    LanguageModel, LanguageModelRequest, LanguageModelRequestMessage, MessageContent,
};

/// Abstraction over contexts that can invoke `LanguageModel::count_tokens`.
pub trait TokenCountApp {
    fn count_tokens_boxed(
        &self,
        model: &Arc<dyn LanguageModel>,
        request: LanguageModelRequest,
    ) -> BoxFuture<'static, Result<u64>>;
}

impl TokenCountApp for App {
    fn count_tokens_boxed(
        &self,
        model: &Arc<dyn LanguageModel>,
        request: LanguageModelRequest,
    ) -> BoxFuture<'static, Result<u64>> {
        model.count_tokens(request, self)
    }
}

impl TokenCountApp for AsyncApp {
    fn count_tokens_boxed(
        &self,
        model: &Arc<dyn LanguageModel>,
        request: LanguageModelRequest,
    ) -> BoxFuture<'static, Result<u64>> {
        // AsyncApp::update gives us an &mut App for the instant needed to obtain the future.
        match self.update(|app| model.count_tokens(request, app)) {
            Ok(fut) => fut,
            Err(e) => futures::future::ready(Err(anyhow!(e))).boxed(),
        }
    }
}

/// Heuristic: approximate tokens from character count.
pub fn heuristic_token_count(messages: &[LanguageModelRequestMessage]) -> usize {
    let chars: usize = messages
        .iter()
        .map(|m| {
            m.content
                .iter()
                .map(|c| match c {
                    MessageContent::Text(t) => t.len(),
                    MessageContent::Thinking { text, .. } => text.len(),
                    MessageContent::RedactedThinking(t) => t.len(),
                    MessageContent::Image(_) => 7,
                    MessageContent::ToolUse(_) => 11,
                    MessageContent::ToolResult(_) => 14,
                })
                .sum::<usize>()
        })
        .sum();
    (chars / 4).max(1)
}

/// Heuristic per-message distribution.
pub fn heuristic_per_message(messages: &[LanguageModelRequestMessage]) -> Vec<usize> {
    messages
        .iter()
        .map(|m| heuristic_token_count(std::slice::from_ref(m)))
        .collect()
}

/// Build a new request with a substituted messages slice.
/// Avoids mutating the original; used for precise counting.
fn rebuild_request(
    base: &LanguageModelRequest,
    msgs: Vec<LanguageModelRequestMessage>,
) -> LanguageModelRequest {
    LanguageModelRequest {
        thread_id: base.thread_id.clone(),
        prompt_id: base.prompt_id.clone(),
        intent: base.intent.clone(),
        mode: base.mode.clone(),
        messages: msgs,
        tools: base.tools.clone(),
        tool_choice: base.tool_choice.clone(),
        stop: base.stop.clone(),
        temperature: base.temperature,
        thinking_allowed: base.thinking_allowed,
    }
}

/// Synchronous (blocking) precise token count for a slice without heuristic fallback.
/// Returns an error if the underlying model count fails or yields zero.
pub fn precise_tokens_for_slice_try(
    model: &Arc<dyn LanguageModel>,
    base: &LanguageModelRequest,
    slice: &[LanguageModelRequestMessage],
    app: &impl TokenCountApp,
) -> Result<usize> {
    if slice.is_empty() {
        return Ok(0);
    }
    let req = rebuild_request(base, slice.to_vec());
    let fut = app.count_tokens_boxed(model, req);
    let count = futures::executor::block_on(fut)?;
    if count == 0 {
        return Err(anyhow!("model returned zero tokens for non-empty slice"));
    }
    Ok(count as usize)
}

/// Synchronous (blocking) per-message precise token counts without heuristic fallback.
/// Performs prefix counts; returns total as sum of returned vector. Errors if any count fails or yields zero unexpectedly.
pub fn precise_per_message_tokens_try(
    model: &Arc<dyn LanguageModel>,
    base: &LanguageModelRequest,
    all: &[LanguageModelRequestMessage],
    app: &impl TokenCountApp,
) -> Result<Vec<usize>> {
    if all.is_empty() {
        return Ok(Vec::new());
    }

    let mut prefix_totals: Vec<usize> = Vec::with_capacity(all.len() + 1);
    prefix_totals.push(0);

    for i in 0..all.len() {
        let slice = &all[..=i];
        let req = rebuild_request(base, slice.to_vec());
        let fut = app.count_tokens_boxed(model, req);
        let total = futures::executor::block_on(fut)?;
        if total == 0 {
            return Err(anyhow!("model returned zero tokens for prefix {}", i));
        }
        prefix_totals.push(total as usize);
    }

    let mut per = Vec::with_capacity(all.len());
    for i in 0..all.len() {
        per.push(prefix_totals[i + 1].saturating_sub(prefix_totals[i]));
    }
    Ok(per)
}

/// Precise token count for an arbitrary slice (fallbacks to heuristic).
pub async fn precise_tokens_for_slice(
    model: &Arc<dyn LanguageModel>,
    base: &LanguageModelRequest,
    slice: &[LanguageModelRequestMessage],
    app: &impl TokenCountApp,
) -> usize {
    if slice.is_empty() {
        return 0;
    }
    let req = rebuild_request(base, slice.to_vec());
    match app.count_tokens_boxed(model, req).await {
        Ok(v) if v > 0 => v as usize,
        _ => heuristic_token_count(slice),
    }
}

/// Compute per-message precise token counts by cumulative prefix subtraction.
/// This is O(n) model.count_tokens calls. For large histories this can be expensive.
pub async fn precise_per_message_tokens(
    model: &Arc<dyn LanguageModel>,
    base: &LanguageModelRequest,
    all: &[LanguageModelRequestMessage],
    app: &impl TokenCountApp,
) -> Result<(Vec<usize>, usize)> {
    if all.is_empty() {
        return Ok((Vec::new(), 0));
    }

    // Sequential prefix approach to preserve ordering and avoid concurrency pressure if provider rate limits.
    let mut prefix_totals: Vec<usize> = Vec::with_capacity(all.len() + 1);
    prefix_totals.push(0);

    for i in 0..all.len() {
        let slice = &all[..=i];
        let t = precise_tokens_for_slice(model, base, slice, app).await;
        prefix_totals.push(t);
    }

    let mut per_message = Vec::with_capacity(all.len());
    for i in 0..all.len() {
        per_message.push(prefix_totals[i + 1].saturating_sub(prefix_totals[i]));
    }
    let total = *prefix_totals.last().unwrap_or(&0);
    Ok((per_message, total))
}

/// Variant that attempts limited parallelism by batching prefix indices.
/// This can reduce latency when provider tolerates concurrent token counts.
/// If batching yields inconsistent results (due to provider variability),
/// fall back to `precise_per_message_tokens`.
pub async fn precise_per_message_tokens_batched(
    model: &Arc<dyn LanguageModel>,
    base: &LanguageModelRequest,
    all: &[LanguageModelRequestMessage],
    app: &impl TokenCountApp,
    batch: usize,
) -> Result<(Vec<usize>, usize)> {
    if all.is_empty() {
        return Ok((Vec::new(), 0));
    }
    if batch == 0 {
        return Err(anyhow!("batch must be > 0"));
    }

    let mut prefix_totals: Vec<(usize, usize)> = Vec::new(); // (index, total_tokens)
    let mut i = 0usize;
    while i < all.len() {
        let end = (i + batch).min(all.len());
        // Build tasks for slice endpoints i..end-1 inclusive
        let mut futures = Vec::with_capacity(end - i);
        for j in i..end {
            let slice = &all[..=j];
            futures.push(precise_tokens_for_slice(model, base, slice, app));
        }
        let results = join_all(futures).await;
        for (offset, t) in results.into_iter().enumerate() {
            prefix_totals.push((i + offset, t));
        }
        i = end;
    }

    // Ensure ordering
    prefix_totals.sort_by_key(|(idx, _)| *idx);

    // Build per-message via differences
    let mut per_message = Vec::with_capacity(all.len());
    let mut last = 0usize;
    for (_, total) in &prefix_totals {
        per_message.push(total.saturating_sub(last));
        last = *total;
    }
    Ok((per_message, last))
}
