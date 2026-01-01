//! Bridge between WebSocket connections and Zed agent threads
//!
//! This module handles the communication between remote WebSocket clients
//! and the local Zed agent system. It uses channels to safely communicate
//! across the thread boundary between tokio (WebSocket) and GPUI (agent).

use anyhow::Result;
use futures::channel::mpsc;
use gpui::{AsyncApp, Entity, WeakEntity};
use serde::{Deserialize, Serialize};

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
}

/// Handle for communicating with an agent thread from a WebSocket
pub struct AgentBridge {
    /// Send messages to the agent (runs on GPUI thread)
    to_agent_tx: mpsc::UnboundedSender<ClientToAgentMessage>,
    /// Receive messages from the agent
    from_agent_rx: mpsc::UnboundedReceiver<AgentToClientMessage>,
}

impl AgentBridge {
    /// Create a new bridge to an agent thread
    ///
    /// This spawns a task on the GPUI executor that processes messages
    /// and communicates with the agent thread.
    pub fn new(cx: &AsyncApp) -> Result<Self> {
        let (to_agent_tx, to_agent_rx) = mpsc::unbounded();
        let (from_agent_tx, from_agent_rx) = mpsc::unbounded();

        // TODO: Spawn task on GPUI executor to handle agent communication
        // For now this is a placeholder
        
        Ok(Self {
            to_agent_tx,
            from_agent_rx,
        })
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
}
