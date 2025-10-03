use crate::{AgentTool, Thread, ToolCallEventStream};
use acp_thread::UserMessageId;
use agent_client_protocol as acp;
use anyhow::{Result, anyhow};
use gpui::{App, Entity, SharedString, Task, WeakEntity};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{OnceLock, RwLock},
};
use uuid::Uuid;

use crate::thread::{Message, UserMessage, UserMessageContent};

/// Global in‑memory store mapping memory handles to serialized context.
/// This is process local and non‑persistent. A future implementation could
/// persist or evict entries based on size limits.
static MEMORY_STORE: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();

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

pub struct MemoryTool {
    thread: WeakEntity<Thread>,
}

impl MemoryTool {
    pub fn new(thread: WeakEntity<Thread>) -> Self {
        Self { thread }
    }

    fn store_range(
        &self,
        thread: &mut Thread,
        start: usize,
        end: usize,
    ) -> Result<(String, usize, usize)> {
        if start > end {
            return Err(anyhow!(
                "start_index ({start}) was greater than end_index ({end})"
            ));
        }
        if end >= thread.messages.len() {
            return Err(anyhow!(
                "end_index ({end}) out of bounds (messages len = {})",
                thread.messages.len()
            ));
        }

        // Collect markdown for selected messages before mutation.
        let mut serialized = String::new();
        for (ix, message) in thread
            .messages
            .iter()
            .enumerate()
            .skip(start)
            .take(end - start + 1)
        {
            if ix > start {
                serialized.push('\n');
            }
            serialized.push_str(&message.to_markdown());
        }

        let message_count = end - start + 1;
        let char_count = serialized.len();

        // Generate a memory handle.
        let session_id = thread.id().0.to_string();
        let handle = format!("mem://{}/{}", session_id, Uuid::new_v4());

        {
            let mut map = memory_store()
                .write()
                .map_err(|_| anyhow!("failed to acquire memory store lock for write"))?;
            map.insert(handle.clone(), serialized);
        }

        // Replace the range with a single synthetic user message referencing the memory handle.
        // Represent as: [[memory:<handle>]]
        let placeholder =
            format!("[[memory: {handle} | {message_count} msgs | {char_count} chars]]");
        let replacement = Message::User(UserMessage {
            id: UserMessageId::new(),
            content: vec![UserMessageContent::Text(placeholder)],
        });

        // Drain and insert.
        thread
            .messages
            .splice(start..=end, std::iter::once(replacement));

        Ok((handle, message_count, char_count))
    }

    fn load_handle(&self, handle: &str) -> Result<String> {
        let map = memory_store()
            .read()
            .map_err(|_| anyhow!("failed to acquire memory store lock for read"))?;
        let content = map
            .get(handle)
            .ok_or_else(|| anyhow!("memory handle not found: {handle}"))?
            .clone();
        Ok(content)
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
                    thread.update(cx, |thread, _cx| {
                        let (handle, msg_count, char_count) =
                            self.store_range(thread, start, end)?;
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
