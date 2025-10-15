use std::sync::Arc;

use anyhow::{Result, anyhow};
use gpui::{App, SharedString, Task, WeakEntity};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::thread::Thread;
use crate::{AgentTool, ToolCallEventStream};

/// Input for the token usage tool.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TokenUsageToolInput {
    /// When true, include per-message token counts (precise if available, otherwise heuristic).
    #[serde(default)]
    pub include_per_message: bool,
    /// When true, include archived memory segment breakdown and aggregate savings.
    #[serde(default)]
    pub include_memory_breakdown: bool,
}

/// Tool output is markdown text.
type TokenUsageToolOutput = String;

/// Tool that reports current thread token usage metrics.
/// (Detailed per-message and memory segment data is unavailable with the current Thread API.)
pub struct TokenUsageTool {
    thread: WeakEntity<Thread>,
}

impl TokenUsageTool {
    pub fn new(thread: WeakEntity<Thread>) -> Self {
        Self { thread }
    }
}

impl AgentTool for TokenUsageTool {
    type Input = TokenUsageToolInput;
    type Output = TokenUsageToolOutput;

    fn name() -> &'static str {
        "token_usage"
    }

    fn kind() -> agent_client_protocol::ToolKind {
        agent_client_protocol::ToolKind::Read
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        match input {
            Ok(cfg) if cfg.include_per_message => "Token usage (detailed)".into(),
            _ => "Token usage".into(),
        }
    }

    fn run(
        self: Arc<Self>,
        input: Self::Input,
        _event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output>> {
        let Some(thread) = self.thread.upgrade() else {
            return Task::ready(Ok("# Token Usage\n\nThread no longer exists.\n".into()));
        };

        // Gather core metrics in a single read.
        // Only the latest overall token usage is available; advanced internal
        // fields (precise active/full, memory segments, per-message counts)
        // are not exposed on the current Thread implementation.
        let usage_opt = thread.read_with(cx, |t, _| t.latest_token_usage());

        // Memory segmentation not available.
        let memory_segments_count = 0usize;
        let memory_saved_tokens: usize = 0;

        // Active vs full tokens (prefer precise for active if available).
        let (active_used, active_max) = match usage_opt {
            Some(ref u) => (u.used_tokens as u64, u.max_tokens as u64),
            None => (0, 0),
        };
        let active_precise = false;

        // No separate "full" context vs "active" distinction available.
        let (full_used, full_max) = (active_used, active_max);

        let active_pct = if active_max > 0 {
            active_used as f64 / active_max as f64 * 100.0
        } else {
            0.0
        };
        let full_pct = if full_max > 0 {
            full_used as f64 / full_max as f64 * 100.0
        } else {
            0.0
        };

        // Heuristic system prompt tokens (overhead not included in active_used above).
        // System prompt heuristic unavailable; treat as zero.
        let system_prompt_tokens = 0u64;

        let combined_active_used = active_used + system_prompt_tokens as u64;
        let combined_active_pct = if active_max > 0 {
            combined_active_used as f64 / active_max as f64 * 100.0
        } else {
            0.0
        };

        // Per-message token counts (precise or heuristic).
        let _per_message_tokens: Option<Vec<usize>> = if input.include_per_message {
            // Not available; inform user later.
            None
        } else {
            None
        };

        // Construct JSON payload.
        let core_json = serde_json::json!({
            "active_tokens_used": active_used,
            "active_tokens_max": active_max,
            "active_usage_pct": (active_pct * 100.0).round() / 100.0,
            "active_is_precise": active_precise,
            "system_prompt_tokens": system_prompt_tokens,
            "active_tokens_used_including_system_prompt": combined_active_used,
            "active_usage_pct_including_system_prompt": (combined_active_pct * 100.0).round() / 100.0,
            "full_tokens_used": full_used,
            "full_tokens_max": full_max,
            "full_usage_pct": (full_pct * 100.0).round() / 100.0,
            "memory_segment_count": memory_segments_count,
            "memory_saved_tokens_estimate": memory_saved_tokens,
        });

        // Build markdown output.
        let mut md = String::new();
        md.push_str("# Token Usage\n\n```json\n");
        match serde_json::to_string_pretty(&core_json) {
            Ok(pretty) => {
                md.push_str(&pretty);
            }
            Err(e) => {
                return Task::ready(Err(anyhow!("failed to serialize core usage: {e}")));
            }
        }
        md.push_str("\n```\n");

        if input.include_per_message {
            md.push_str("\n## Per-Message Tokens\n\nDetailed per-message token data is not available in the current build.\n");
        }

        if input.include_memory_breakdown {
            md.push_str("\n## Memory Segments\n\nNo memory segmentation data is available in the current build.\n");
        }

        Task::ready(Ok(md))
    }
}
