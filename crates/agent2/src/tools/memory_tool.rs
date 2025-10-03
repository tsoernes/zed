use crate::{AgentTool, Thread, ToolCallEventStream};
use acp_thread::UserMessageId;
use agent_client_protocol as acp;
use anyhow::{Result, anyhow};
use db::kvp::KEY_VALUE_STORE;
use gpui::{App, Context, SharedString, Task, WeakEntity};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{OnceLock, RwLock},
};
use uuid::Uuid;

use crate::thread::{Message, UserMessage, UserMessageContent};

/// Global in-memory store mapping memory handles to serialized context.
/// Now supplemented by persistence via KEY_VALUE_STORE.
/// Values are written asynchronously; reads fall back to persistence if not present in memory.
static MEMORY_STORE: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();

const MEMORY_KV_PREFIX: &str = "agent_memory::";

fn memory_store() -> &'static RwLock<HashMap<String, String>> {
    MEMORY_STORE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// The operation to perform.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryOperation {
    /// Store a contiguous range of existing thread messages (by 0-based inclusive indices)
    /// into memory and replace them in the thread with a single reference marker.
    Store,
    /// Load (retrieve) previously stored content by its memory handle.
    Load,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct MemoryToolInput {
    /// Operation: "store" or "load".
    operation: MemoryOperation,
    /// Inclusive start index of the message range to store. Required for store.
    start_index: Option<usize>,
    /// Inclusive end index of the message range to store. Required for store.
    end_index: Option<usize>,
    /// Memory handle to load (mem://<session>/<id>). Required for load.
    memory_handle: Option<String>,
}

/// Tool for compacting conversation history when nearing context/token limits.
///
/// Use this to:
/// - Store a contiguous range of older messages in a persistent memory handle (operation = "store")
///   which replaces them in the live history with a lightweight placeholder marker
/// - Retrieve previously stored content by handle (operation = "load") without re‑expanding it
///
/// This helps prevent the model from running out of context window space while still allowing
/// explicit retrieval of archived details later.
pub struct MemoryTool {
    thread: WeakEntity<Thread>,
}

impl MemoryTool {
    pub fn new(thread: WeakEntity<Thread>) -> Self {
        Self { thread }
    }

    // (Helper methods removed; now using Thread's public helper methods)

    fn kv_key(handle: &str) -> String {
        format!("{MEMORY_KV_PREFIX}{handle}")
    }

    fn store_range(
        &self,
        thread: &mut Thread,
        cx: &mut Context<Thread>,
        start: usize,
        end: usize,
    ) -> Result<(String, usize, usize)> {
        if start > end {
            return Err(anyhow!(
                "start_index ({start}) was greater than end_index ({end})"
            ));
        }
        // Delegate length check via helper to avoid direct field semantics.
        if end >= thread.message_len() {
            return Err(anyhow!(
                "end_index ({end}) out of bounds (messages len = {})",
                thread.message_len()
            ));
        }

        // Use thread helper to serialize the inclusive range.
        let (serialized, message_count, char_count) = thread.serialize_range(start, end)?;

        // Generate a memory handle.
        let session_id = thread.id().0.to_string();
        let handle = format!("mem://{}/{}", session_id, Uuid::new_v4());

        {
            let mut map = memory_store()
                .write()
                .map_err(|_| anyhow!("failed to acquire memory store lock for write"))?;
            map.insert(handle.clone(), serialized.clone());
        }

        // Persist asynchronously (skip in tests).
        if !cfg!(any(feature = "test-support", test)) {
            let key = Self::kv_key(&handle);
            cx.background_spawn(async move {
                let _ = KEY_VALUE_STORE.write_kvp(key, serialized).await;
            })
            .detach();
        }

        // Replace the range with a single synthetic user message referencing the memory handle.
        let placeholder =
            format!("[[memory: {handle} | {message_count} msgs | {char_count} chars]]");
        let replacement = Message::User(UserMessage {
            id: UserMessageId::new(),
            content: vec![UserMessageContent::Text(placeholder)],
        });

        thread.replace_range_with_message(start, end, replacement)?;

        Ok((handle, message_count, char_count))
    }

    fn load_handle(&self, handle: &str) -> Result<String> {
        // First check in-memory.
        if let Ok(map) = memory_store().read() {
            if let Some(val) = map.get(handle) {
                return Ok(val.clone());
            }
        }
        // Fallback to persistence.
        let key = Self::kv_key(handle);
        if let Some(serialized) = KEY_VALUE_STORE.read_kvp(key).log_err().flatten() {
            // Cache in memory for faster subsequent access.
            if let Ok(mut map) = memory_store().write() {
                map.insert(handle.to_string(), serialized.clone());
            }
            return Ok(serialized);
        }
        Err(anyhow!("memory handle not found: {handle}"))
    }
}

impl AgentTool for MemoryTool {
    type Input = MemoryToolInput;
    type Output = String;

    fn name() -> &'static str {
        "memory"
    }

    fn kind() -> acp::ToolKind {
        // No dedicated kind exists, so categorize as Other.
        acp::ToolKind::Other
    }

    fn description(&self) -> SharedString {
        "Use this tool to shorten conversation history when nearing context/token limits: \
store a contiguous range of older messages into a compact memory handle (operation=\"store\") \
which replaces them with a small placeholder, or retrieve previously stored content on demand \
(operation=\"load\")."
            .into()
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        match input {
            Ok(i) => match i.operation {
                MemoryOperation::Store => "Store context range".into(),
                MemoryOperation::Load => "Load stored memory".into(),
            },
            Err(_) => "Memory operation".into(),
        }
    }

    fn run(
        self: std::sync::Arc<Self>,
        input: Self::Input,
        event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<String>> {
        match input.operation {
            MemoryOperation::Store => {
                let Some(thread) = self.thread.upgrade() else {
                    return Task::ready(Err(anyhow!("thread no longer exists")));
                };
                let start = input
                    .start_index
                    .ok_or_else(|| anyhow!("start_index is required for store"))?;
                let end = input
                    .end_index
                    .ok_or_else(|| anyhow!("end_index is required for store"))?;
                cx.update(|cx| {
                    thread.update(cx, |thread, thread_cx| {
                        let (handle, msg_count, char_count) =
                            self.store_range(thread, thread_cx, start, end)?;
                        let output = format!(
                            "Stored {msg_count} message(s) ({char_count} chars) in memory handle: {handle}\n\
                             The selected range [{start}..{end}] was replaced by a memory reference."
                        );
                        event_stream.update_fields(acp::ToolCallUpdateFields {
                            content: Some(vec![output.clone().into()]),
                            ..Default::default()
                        });
                        Ok(output)
                    })
                })
            }
            MemoryOperation::Load => {
                let handle = input
                    .memory_handle
                    .ok_or_else(|| anyhow!("memory_handle is required for load"))?;
                let content = match self.load_handle(&handle) {
                    Ok(c) => c,
                    Err(err) => return Task::ready(Err(err)),
                };
                let output = format!("Content from {handle}:\n\n{content}");
                event_stream.update_fields(acp::ToolCallUpdateFields {
                    content: Some(vec![output.clone().into()]),
                    ..Default::default()
                });
                Task::ready(Ok(output))
            }
        }
    }
}
