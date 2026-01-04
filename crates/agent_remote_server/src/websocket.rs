use crate::agent_bridge::{AgentBridge, AgentToClientMessage, ClientToAgentMessage};
use anyhow::{Result, anyhow};
use axum::extract::ws::{Message, WebSocket};
use futures::{SinkExt, StreamExt};
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Message from client to server
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ClientMessage {
    /// Send a chat message to the agent
    Chat { content: String },
    /// Request thread history
    GetHistory,
    /// Cancel ongoing agent operation
    Cancel,
    /// Ping to keep connection alive
    Ping,
}

/// Message from server to client
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ServerMessage {
    /// Connection established
    Connected { session_id: String },
    /// Agent response text chunk
    TextChunk { entry_index: usize, content: String },
    /// Agent started using a tool
    ToolStart {
        entry_index: usize,
        tool_call_id: String,
        tool_name: String,
        tool_input: serde_json::Value,
    },
    /// Tool execution result
    ToolResult {
        entry_index: usize,
        tool_call_id: String,
        tool_name: String,
        result: String,
        error: Option<String>,
    },
    /// Agent finished responding
    ResponseComplete { entry_index: usize },
    /// Error occurred
    Error { message: String },
    /// Pong response
    Pong,
    /// History entry
    HistoryEntry { role: String, content: String },
    /// Token usage update
    TokenUsageUpdate { used_tokens: u64, max_tokens: u64 },
    /// Model information update
    ModelInfoUpdate {
        model_id: String,
        model_name: String,
    },
}

/// Handle a WebSocket connection with agent integration
pub async fn handle_websocket(socket: WebSocket, agent_bridge: AgentBridge) {
    let (sender, mut receiver) = socket.split();
    let sender = Arc::new(Mutex::new(sender));

    // Create a connection handle for this WebSocket
    let mut connection = agent_bridge.create_connection();
    let connection_id = connection.id();

    info!("WebSocket connection established: {}", connection_id);

    // Send connected message
    {
        let mut sender_guard = sender.lock().await;
        if let Err(e) = send_message(
            &mut sender_guard,
            ServerMessage::Connected {
                session_id: connection_id.to_string(),
            },
        )
        .await
        {
            error!("Failed to send connected message: {:?}", e);
            return;
        }
    }

    // Request initial state (model info, token usage) for this new client
    if let Err(e) = agent_bridge.send(ClientToAgentMessage::GetInitialState) {
        error!("Failed to request initial state: {:?}", e);
    }

    // Spawn a task to forward agent messages to the WebSocket
    let sender_clone = sender.clone();
    let response_task = tokio::spawn(async move {
        while let Some(agent_msg) = connection.recv().await {
            let server_msg = match agent_msg {
                AgentToClientMessage::TextChunk {
                    entry_index,
                    content,
                } => ServerMessage::TextChunk {
                    entry_index,
                    content,
                },
                AgentToClientMessage::ToolStart {
                    entry_index,
                    tool_call_id,
                    tool_name,
                    tool_input,
                } => ServerMessage::ToolStart {
                    entry_index,
                    tool_call_id,
                    tool_name,
                    tool_input,
                },
                AgentToClientMessage::ToolResult {
                    entry_index,
                    tool_call_id,
                    tool_name,
                    result,
                    error,
                } => ServerMessage::ToolResult {
                    entry_index,
                    tool_call_id,
                    tool_name,
                    result,
                    error,
                },
                AgentToClientMessage::ResponseComplete { entry_index } => {
                    ServerMessage::ResponseComplete { entry_index }
                }
                AgentToClientMessage::Error { message } => ServerMessage::Error { message },
                AgentToClientMessage::HistoryEntry { role, content } => {
                    ServerMessage::HistoryEntry { role, content }
                }
                AgentToClientMessage::TokenUsageUpdate {
                    used_tokens,
                    max_tokens,
                } => ServerMessage::TokenUsageUpdate {
                    used_tokens,
                    max_tokens,
                },
                AgentToClientMessage::ModelInfoUpdate {
                    model_id,
                    model_name,
                } => ServerMessage::ModelInfoUpdate {
                    model_id,
                    model_name,
                },
            };

            let mut sender_guard = sender_clone.lock().await;
            if let Err(e) = send_message(&mut sender_guard, server_msg).await {
                error!("Failed to send agent message to client: {:?}", e);
                break;
            }
        }
    });

    // Process incoming messages from the client
    while let Some(msg) = receiver.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                debug!("Received text message: {}", text);

                match serde_json::from_str::<ClientMessage>(&text) {
                    Ok(client_msg) => {
                        if let Err(e) =
                            handle_client_message(client_msg, &sender, &agent_bridge).await
                        {
                            error!("Error handling message: {:?}", e);
                            let mut sender_guard = sender.lock().await;
                            let _ = send_message(
                                &mut sender_guard,
                                ServerMessage::Error {
                                    message: e.to_string(),
                                },
                            )
                            .await;
                        }
                    }
                    Err(e) => {
                        warn!("Failed to parse client message: {:?}", e);
                        let mut sender_guard = sender.lock().await;
                        let _ = send_message(
                            &mut sender_guard,
                            ServerMessage::Error {
                                message: format!("Invalid message format: {}", e),
                            },
                        )
                        .await;
                    }
                }
            }
            Ok(Message::Binary(_)) => {
                debug!("Received binary message (ignoring)");
            }
            Ok(Message::Ping(_)) => {
                debug!("Received ping");
            }
            Ok(Message::Pong(_)) => {
                debug!("Received pong");
            }
            Ok(Message::Close(_)) => {
                info!("Client closed connection: {}", connection_id);
                break;
            }
            Err(e) => {
                error!("WebSocket error: {:?}", e);
                break;
            }
        }
    }

    // Clean up
    response_task.abort();
    info!("WebSocket connection closed: {}", connection_id);
}

/// Handle a client message
async fn handle_client_message(
    message: ClientMessage,
    sender: &Arc<Mutex<futures::stream::SplitSink<WebSocket, Message>>>,
    agent_bridge: &AgentBridge,
) -> Result<()> {
    match message {
        ClientMessage::Chat { content } => {
            info!("Processing chat message: {}", content);

            // Send to agent via bridge
            agent_bridge.send(ClientToAgentMessage::Chat { content })?;

            // The response will be streamed back via the response task
        }
        ClientMessage::GetHistory => {
            info!("Fetching conversation history");

            // Request history from agent
            agent_bridge.send(ClientToAgentMessage::GetHistory)?;

            // History entries will be streamed back via the response task
        }
        ClientMessage::Cancel => {
            info!("Cancelling agent operation");

            // Send cancellation to agent
            agent_bridge.send(ClientToAgentMessage::Cancel)?;
        }
        ClientMessage::Ping => {
            let mut sender_guard = sender.lock().await;
            send_message(&mut sender_guard, ServerMessage::Pong).await?;
        }
    }
    Ok(())
}

/// Helper to send a server message
async fn send_message(
    sender: &mut futures::stream::SplitSink<WebSocket, Message>,
    message: ServerMessage,
) -> Result<()> {
    let json = serde_json::to_string(&message)?;
    sender
        .send(Message::Text(json))
        .await
        .map_err(|e| anyhow!("Failed to send message: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_message_serialization() {
        let msg = ClientMessage::Chat {
            content: "Hello".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"chat\""));
        assert!(json.contains("\"content\":\"Hello\""));
    }

    #[test]
    fn test_client_message_deserialization() {
        let json = r#"{"type":"chat","content":"Hello"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::Chat { content } => assert_eq!(content, "Hello"),
            _ => panic!("Wrong message type"),
        }
    }

    #[test]
    fn test_server_message_serialization() {
        let msg = ServerMessage::TextChunk {
            content: "Response".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"textChunk\""));
        assert!(json.contains("\"content\":\"Response\""));
    }

    #[test]
    fn test_server_message_connected() {
        let msg = ServerMessage::Connected {
            session_id: "test-123".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"connected\""));
        assert!(json.contains("\"session_id\":\"test-123\""));
    }

    #[test]
    fn test_server_message_error() {
        let msg = ServerMessage::Error {
            message: "Test error".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"error\""));
        assert!(json.contains("\"message\":\"Test error\""));
    }

    #[test]
    fn test_history_entry_serialization() {
        let msg = ServerMessage::HistoryEntry {
            role: "user".to_string(),
            content: "Test message".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"historyEntry\""));
        assert!(json.contains("\"role\":\"user\""));
        assert!(json.contains("\"content\":\"Test message\""));
    }
}
