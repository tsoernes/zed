//! Bridge between WebSocket connections and Zed agent threads
//!
//! This module handles the communication between remote WebSocket clients
//! and the local Zed agent system. It uses channels to safely communicate
//! across the thread boundary between tokio (WebSocket) and GPUI (agent).

use acp_thread::{AcpThread, AcpThreadEvent, AgentThreadEntry};
use agent_client_protocol as acp;
use anyhow::Result;
use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{App, Entity, Subscription, WeakEntity};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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

/// Shared state for the agent bridge that can handle multiple connections
struct BridgeState {
    /// Send messages to the agent (runs on GPUI thread)
    to_agent_tx: mpsc::UnboundedSender<ClientToAgentMessage>,
    /// Broadcast channel for agent responses to all connected clients
    clients: Arc<Mutex<HashMap<uuid::Uuid, mpsc::UnboundedSender<AgentToClientMessage>>>>,
}

/// Handle for communicating with an agent thread from a WebSocket
/// This can be cloned and shared across multiple connections
#[derive(Clone)]
pub struct AgentBridge {
    state: Arc<BridgeState>,
}

/// Per-connection handle for receiving messages
pub struct ConnectionHandle {
    connection_id: uuid::Uuid,
    rx: mpsc::UnboundedReceiver<AgentToClientMessage>,
    bridge: AgentBridge,
}

impl AgentBridge {
    /// Create a new bridge to an agent thread
    ///
    /// This spawns a task on the GPUI executor that processes messages
    /// and communicates with the agent thread.
    pub fn new(acp_thread: WeakEntity<AcpThread>, cx: &mut App) -> Result<Self> {
        let (to_agent_tx, mut to_agent_rx) = mpsc::unbounded();
        let clients = Arc::new(Mutex::new(HashMap::new()));

        let clients_clone = clients.clone();

        // Spawn task on GPUI executor to handle agent communication
        let _task = cx.spawn(|mut cx| async move {
            // Subscribe to thread events
            let _subscription = acp_thread
                .update(&mut cx, |_thread, cx| {
                    let clients = clients_clone.clone();
                    cx.subscribe(&acp_thread, move |_thread, event, _cx| {
                        Self::handle_thread_event(event, clients.clone());
                    })
                })
                .ok();

            // Process incoming messages from WebSocket clients
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
                            Self::broadcast_to_clients(
                                &clients_clone,
                                AgentToClientMessage::Error {
                                    message: format!("Failed to send message: {}", e),
                                },
                            );
                        }
                    }
                    ClientToAgentMessage::GetHistory => {
                        // Fetch conversation history
                        if let Ok(entries) =
                            acp_thread.read_with(&cx, |thread, _cx| thread.entries().to_vec())
                        {
                            for entry in entries {
                                if let Some(msg) = Self::history_entry_to_message(&entry) {
                                    Self::broadcast_to_clients(&clients_clone, msg);
                                }
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

        let state = BridgeState {
            to_agent_tx,
            clients,
        };

        Ok(Self {
            state: Arc::new(state),
        })
    }

    /// Create a new connection handle for a WebSocket client
    pub fn create_connection(&self) -> ConnectionHandle {
        let connection_id = uuid::Uuid::new_v4();
        let (tx, rx) = mpsc::unbounded();

        self.state.clients.lock().insert(connection_id, tx);

        ConnectionHandle {
            connection_id,
            rx,
            bridge: self.clone(),
        }
    }

    /// Send a message to the agent
    pub fn send(&self, message: ClientToAgentMessage) -> Result<()> {
        self.state
            .to_agent_tx
            .unbounded_send(message)
            .map_err(|e| anyhow::anyhow!("Failed to send to agent: {}", e))
    }

    /// Broadcast a message to all connected clients
    fn broadcast_to_clients(
        clients: &Arc<Mutex<HashMap<uuid::Uuid, mpsc::UnboundedSender<AgentToClientMessage>>>>,
        message: AgentToClientMessage,
    ) {
        let mut clients = clients.lock();
        // Remove disconnected clients
        clients.retain(|_id, tx| !tx.is_closed());

        // Send to all connected clients
        for tx in clients.values() {
            let _ = tx.unbounded_send(message.clone());
        }
    }

    /// Handle thread events and forward to WebSocket
    fn handle_thread_event(
        event: &AcpThreadEvent,
        clients: Arc<Mutex<HashMap<uuid::Uuid, mpsc::UnboundedSender<AgentToClientMessage>>>>,
    ) {
        match event {
            AcpThreadEvent::NewEntry | AcpThreadEvent::EntryUpdated(_) => {
                // Entry updated - clients can poll or we could send the actual content
            }
            AcpThreadEvent::Stopped => {
                Self::broadcast_to_clients(&clients, AgentToClientMessage::ResponseComplete);
            }
            AcpThreadEvent::Error => {
                Self::broadcast_to_clients(
                    &clients,
                    AgentToClientMessage::Error {
                        message: "Agent encountered an error".to_string(),
                    },
                );
            }
            AcpThreadEvent::Refusal => {
                Self::broadcast_to_clients(
                    &clients,
                    AgentToClientMessage::Error {
                        message: "Agent refused to respond to this request".to_string(),
                    },
                );
            }
            _ => {
                // Other events don't need special handling for remote clients
            }
        }
    }

    /// Convert a history entry to a message, if applicable
    fn history_entry_to_message(entry: &AgentThreadEntry) -> Option<AgentToClientMessage> {
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

                Some(AgentToClientMessage::HistoryEntry {
                    role: "user".to_string(),
                    content,
                })
            }
            AgentThreadEntry::AssistantMessage(msg) => {
                // Extract text from assistant message
                let content = format!("Assistant message: {:?}", msg.chunks.len());
                Some(AgentToClientMessage::HistoryEntry {
                    role: "assistant".to_string(),
                    content,
                })
            }
            _ => None,
        }
    }
}

impl ConnectionHandle {
    /// Get the connection ID
    pub fn id(&self) -> uuid::Uuid {
        self.connection_id
    }

    /// Send a message to the agent
    pub fn send(&self, message: ClientToAgentMessage) -> Result<()> {
        self.bridge.send(message)
    }

    /// Try to receive a message from the agent (non-blocking)
    pub fn try_recv(&mut self) -> Option<AgentToClientMessage> {
        self.rx.try_next().ok().flatten()
    }

    /// Receive next message from agent (blocking)
    pub async fn recv(&mut self) -> Option<AgentToClientMessage> {
        self.rx.next().await
    }
}

impl Drop for ConnectionHandle {
    fn drop(&mut self) {
        // Remove this connection from the clients map
        self.bridge.state.clients.lock().remove(&self.connection_id);
    }
}
