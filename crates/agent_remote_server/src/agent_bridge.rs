//! Bridge between WebSocket connections and Zed agent threads
//!
//! This module handles the communication between remote WebSocket clients
//! and the local Zed agent system. It uses channels to safely communicate
//! across the thread boundary between tokio (WebSocket) and GPUI (agent).

use acp_thread::{AcpThread, AcpThreadEvent, AgentThreadEntry};
use agent_client_protocol as acp;
use anyhow::{Context as _, Result};
use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{App, AsyncApp, Context, Entity, Subscription, Task, WeakEntity};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Message sent from WebSocket client to agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientToAgentMessage {
    /// User wants to send a chat message
    Chat { content: String },
    /// Request conversation history
    GetHistory,
    /// Cancel the current agent operation
    Cancel,
}

/// Message sent from agent back to WebSocket client
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentToClientMessage {
    /// Text chunk from agent response (streaming)
    TextChunk { content: String },
    /// Agent started using a tool
    ToolStart {
        tool_name: String,
        tool_input: serde_json::Value,
    },
    /// Tool execution completed
    ToolResult {
        tool_name: String,
        result: String,
        error: Option<String>,
    },
    /// Agent finished its response
    ResponseComplete,
    /// An error occurred
    Error { message: String },
    /// History entry
    HistoryEntry { role: String, content: String },
}

/// Handle for communicating with an agent thread from a WebSocket
pub struct AgentBridge {
    /// Send messages to the agent (runs on GPUI thread)
    to_agent_tx: mpsc::UnboundedSender<ClientToAgentMessage>,
    /// Receive messages from the agent
    from_agent_rx: mpsc::UnboundedReceiver<AgentToClientMessage>,
    /// Keep subscription alive
    _subscription: Arc<parking_lot::Mutex<Option<Subscription>>>,
}

impl AgentBridge {
    /// Create a new bridge to an agent thread
    ///
    /// This spawns a task on the GPUI executor that processes messages
    /// and communicates with the agent thread.
    pub fn new(acp_thread: WeakEntity<AcpThread>, cx: &mut App) -> Result<Self> {
        let (to_agent_tx, mut to_agent_rx) = mpsc::unbounded();
        let (from_agent_tx, from_agent_rx) = mpsc::unbounded();

        let subscription = Arc::new(parking_lot::Mutex::new(None));
        let subscription_clone = subscription.clone();

        // Spawn task on GPUI executor to handle agent communication
        let _task = cx.spawn(|mut cx| async move {
            // Subscribe to thread events
            let sub = acp_thread
                .update(&mut cx, |thread, cx| {
                    let from_agent_tx = from_agent_tx.clone();
                    cx.subscribe(&Entity::downgrade(thread), move |_thread, event, _cx| {
                        Self::handle_thread_event(event, from_agent_tx.clone());
                    })
                })
                .ok();

            if let Some(sub) = sub {
                *subscription_clone.lock() = Some(sub);
            }

            // Process incoming messages from WebSocket client
            while let Some(msg) = to_agent_rx.next().await {
                match msg {
                    ClientToAgentMessage::Chat { content } => {
                        // Convert to ContentBlock
                        let blocks =
                            vec![acp::ContentBlock::Text(acp::TextContent { text: content })];

                        // Send to agent thread
                        let result = acp_thread
                            .update(&mut cx, |thread, cx| thread.send(blocks, cx))
                            .and_then(|future| async move { future.await }.now_or_never())
                            .flatten();

                        if let Some(Err(e)) = result {
                            let _ = from_agent_tx.unbounded_send(AgentToClientMessage::Error {
                                message: format!("Failed to send message: {}", e),
                            });
                        }
                    }
                    ClientToAgentMessage::GetHistory => {
                        // Fetch conversation history
                        if let Ok(entries) =
                            acp_thread.read_with(&cx, |thread, _cx| thread.entries().to_vec())
                        {
                            for entry in entries {
                                Self::send_history_entry(&entry, &from_agent_tx);
                            }
                        }
                    }
                    ClientToAgentMessage::Cancel => {
                        // Attempt to cancel the current operation
                        let _ = acp_thread.update(&mut cx, |thread, cx| {
                            thread.cancel(cx);
                        });
                    }
                }
            }
        });

        Ok(Self {
            to_agent_tx,
            from_agent_rx,
            _subscription: subscription,
        })
    }

    /// Handle thread events and forward to WebSocket
    fn handle_thread_event(
        event: &AcpThreadEvent,
        from_agent_tx: mpsc::UnboundedSender<AgentToClientMessage>,
    ) {
        match event {
            AcpThreadEvent::NewEntry | AcpThreadEvent::EntryUpdated(_) => {
                // Entry updated - client will poll for updates
                // For now, we signal completion when processing stops
            }
            AcpThreadEvent::Stopped => {
                let _ = from_agent_tx.unbounded_send(AgentToClientMessage::ResponseComplete);
            }
            AcpThreadEvent::Error => {
                let _ = from_agent_tx.unbounded_send(AgentToClientMessage::Error {
                    message: "Agent encountered an error".to_string(),
                });
            }
            AcpThreadEvent::Refusal => {
                let _ = from_agent_tx.unbounded_send(AgentToClientMessage::Error {
                    message: "Agent refused to respond to this request".to_string(),
                });
            }
            _ => {
                // Other events don't need special handling for remote clients
            }
        }
    }

    /// Send a history entry to the client
    fn send_history_entry(
        entry: &AgentThreadEntry,
        from_agent_tx: &mpsc::UnboundedSender<AgentToClientMessage>,
    ) {
        match entry {
            AgentThreadEntry::UserMessage(msg) => {
                let content = msg
                    .chunks
                    .iter()
                    .filter_map(|block| {
                        if let acp::ContentBlock::Text(text) = block {
                            Some(text.text.clone())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                let _ = from_agent_tx.unbounded_send(AgentToClientMessage::HistoryEntry {
                    role: "user".to_string(),
                    content,
                });
            }
            AgentThreadEntry::AssistantMessage(msg) => {
                // Extract text from assistant message
                let content = format!("Assistant message: {:?}", msg.chunks.len());
                let _ = from_agent_tx.unbounded_send(AgentToClientMessage::HistoryEntry {
                    role: "assistant".to_string(),
                    content,
                });
            }
            _ => {
                // Skip other entry types for now
            }
        }
    }

    /// Send a message to the agent
    pub fn send(&mut self, message: ClientToAgentMessage) -> Result<()> {
        self.to_agent_tx
            .unbounded_send(message)
            .map_err(|e| anyhow::anyhow!("Failed to send to agent: {}", e))
    }

    /// Try to receive a message from the agent (non-blocking)
    pub fn try_recv(&mut self) -> Option<AgentToClientMessage> {
        self.from_agent_rx.try_next().ok().flatten()
    }

    /// Receive next message from agent (blocking)
    pub async fn recv(&mut self) -> Option<AgentToClientMessage> {
        self.from_agent_rx.next().await
    }
}
