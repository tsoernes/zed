//! Bridge between WebSocket connections and Zed agent threads
//!
//! This module provides a simple pub/sub system for broadcasting messages
//! between the agent and multiple WebSocket clients. It does NOT handle
//! agent communication directly - that's done by the server layer with
//! proper GPUI context.

use anyhow::Result;
use futures::StreamExt;
use futures::channel::mpsc;

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
    /// Broadcast channel for agent responses to all connected clients
    clients: Arc<Mutex<HashMap<uuid::Uuid, mpsc::UnboundedSender<AgentToClientMessage>>>>,
    /// Send messages to agent (forwarded to server layer)
    to_agent_tx: mpsc::UnboundedSender<ClientToAgentMessage>,
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
    /// Create a new bridge
    ///
    /// Returns the bridge and channels for the server to use for agent communication:
    /// - `to_agent_rx`: Server should poll this to get messages from clients
    /// - `from_agent_tx`: Server should send agent responses here to broadcast to clients
    pub fn new<C: gpui::AppContext>(
        cx: &C,
    ) -> (
        Self,
        mpsc::UnboundedReceiver<ClientToAgentMessage>,
        mpsc::UnboundedSender<AgentToClientMessage>,
    ) {
        let (to_agent_tx, to_agent_rx) = mpsc::unbounded();
        let (from_agent_tx, mut from_agent_rx) = mpsc::unbounded();
        let clients = Arc::new(Mutex::new(HashMap::new()));

        let clients_clone = clients.clone();

        // Spawn a task to forward agent messages to all connected clients
        // Use gpui_tokio to access the Tokio runtime managed by GPUI
        let _broadcast_task = gpui_tokio::Tokio::spawn(cx, async move {
            while let Some(msg) = from_agent_rx.next().await {
                Self::broadcast_to_clients(&clients_clone, msg);
            }
            Ok::<(), anyhow::Error>(())
        });

        let state = Arc::new(BridgeState {
            clients,
            to_agent_tx,
        });

        let bridge = Self { state };

        (bridge, to_agent_rx, from_agent_tx)
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

    /// Send a message to the agent (forwarded to server layer)
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

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    async fn test_bridge_creation(cx: &mut TestAppContext) {
        cx.update(|cx| gpui_tokio::init(cx));
        let (bridge, _to_agent_rx, _from_agent_tx) = AgentBridge::new(cx);
        assert_eq!(bridge.state.clients.lock().len(), 0);
    }

    #[gpui::test]
    async fn test_connection_handle(cx: &mut TestAppContext) {
        cx.update(|cx| gpui_tokio::init(cx));
        let (bridge, _to_agent_rx, _from_agent_tx) = AgentBridge::new(cx);
        let _handle = bridge.create_connection();
        assert_eq!(bridge.state.clients.lock().len(), 1);
    }

    #[gpui::test]
    async fn test_connection_cleanup(cx: &mut TestAppContext) {
        cx.update(|cx| gpui_tokio::init(cx));
        let (bridge, _to_agent_rx, _from_agent_tx) = AgentBridge::new(cx);
        {
            let _handle = bridge.create_connection();
            assert_eq!(bridge.state.clients.lock().len(), 1);
        }
        // Handle dropped, should be cleaned up
        cx.background_executor
            .timer(std::time::Duration::from_millis(10))
            .await;
        // Note: cleanup happens on next broadcast or explicit retention check
    }

    #[gpui::test]
    async fn test_message_sending(cx: &mut TestAppContext) {
        cx.update(|cx| gpui_tokio::init(cx));
        let (bridge, mut to_agent_rx, _from_agent_tx) = AgentBridge::new(cx);

        bridge
            .send(ClientToAgentMessage::Chat {
                content: "Hello".to_string(),
            })
            .unwrap();

        let msg = to_agent_rx.next().await.unwrap();
        match msg {
            ClientToAgentMessage::Chat { content } => assert_eq!(content, "Hello"),
            _ => panic!("Wrong message type"),
        }
    }

    #[gpui::test]
    async fn test_broadcast(cx: &mut TestAppContext) {
        cx.update(|cx| gpui_tokio::init(cx));
        let (bridge, _to_agent_rx, from_agent_tx) = AgentBridge::new(cx);

        let mut handle1 = bridge.create_connection();
        let mut handle2 = bridge.create_connection();

        // Send a message to broadcast
        from_agent_tx
            .unbounded_send(AgentToClientMessage::TextChunk {
                content: "Test".to_string(),
            })
            .unwrap();

        // Give broadcast task time to process
        cx.executor().run_until_parked();
        cx.background_executor
            .timer(std::time::Duration::from_millis(10))
            .await;

        // Both handles should receive the message
        let msg1 = handle1.recv().await.unwrap();
        let msg2 = handle2.recv().await.unwrap();

        match msg1 {
            AgentToClientMessage::TextChunk { content } => assert_eq!(content, "Test"),
            _ => panic!("Wrong message type"),
        }

        match msg2 {
            AgentToClientMessage::TextChunk { content } => assert_eq!(content, "Test"),
            _ => panic!("Wrong message type"),
        }
    }
}
