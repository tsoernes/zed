use crate::agent_bridge::{AgentBridge, AgentToClientMessage, ClientToAgentMessage};
use acp_thread::AcpThread;
use anyhow::{Result, anyhow};
use axum::extract::ws::{Message, WebSocket};
use futures::{SinkExt, StreamExt};
use gpui::{App, WeakEntity};
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};

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
    TextChunk { content: String },
    /// Agent started using a tool
    ToolStart {
        tool_name: String,
        tool_input: serde_json::Value,
    },
    /// Tool execution result
    ToolResult {
        tool_name: String,
        result: String,
        error: Option<String>,
    },
    /// Agent finished responding
    ResponseComplete,
    /// Error occurred
    Error { message: String },
    /// Pong response
    Pong,
    /// History entry
    HistoryEntry { role: String, content: String },
}

/// Handle a WebSocket connection with agent integration
pub async fn handle_websocket(socket: WebSocket, acp_thread: WeakEntity<AcpThread>) {
    let (mut sender, mut receiver) = socket.split();

    // Generate session ID
    let session_id = uuid::Uuid::new_v4().to_string();
    info!("WebSocket connection established: {}", session_id);

    // Send connected message
    if let Err(e) = send_message(
        &mut sender,
        ServerMessage::Connected {
            session_id: session_id.clone(),
        },
    )
    .await
    {
        error!("Failed to send connected message: {:?}", e);
        return;
    }

    // Create agent bridge
    // Note: We need to create the bridge on the GPUI thread
    // For now, we'll handle messages directly and integrate bridge later
    // when we have proper GPUI context available
    info!("WebSocket handler started (agent bridge integration pending)");

    // Process incoming messages
    while let Some(msg) = receiver.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                debug!("Received text message: {}", text);

                match serde_json::from_str::<ClientMessage>(&text) {
                    Ok(client_msg) => {
                        if let Err(e) =
                            handle_client_message(client_msg, &mut sender, &acp_thread).await
                        {
                            error!("Error handling message: {:?}", e);
                            let _ = send_message(
                                &mut sender,
                                ServerMessage::Error {
                                    message: e.to_string(),
                                },
                            )
                            .await;
                        }
                    }
                    Err(e) => {
                        warn!("Failed to parse client message: {:?}", e);
                        let _ = send_message(
                            &mut sender,
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
                info!("Client closed connection: {}", session_id);
                break;
            }
            Err(e) => {
                error!("WebSocket error: {:?}", e);
                break;
            }
        }
    }

    info!("WebSocket connection closed: {}", session_id);
}

/// Handle a client message
async fn handle_client_message(
    message: ClientMessage,
    sender: &mut futures::stream::SplitSink<WebSocket, Message>,
    _acp_thread: &WeakEntity<AcpThread>,
) -> Result<()> {
    match message {
        ClientMessage::Chat { content } => {
            handle_chat_message(content, sender).await?;
        }
        ClientMessage::GetHistory => {
            handle_get_history(sender).await?;
        }
        ClientMessage::Cancel => {
            // TODO: Implement cancellation via agent bridge
            send_message(
                sender,
                ServerMessage::Error {
                    message: "Cancellation not yet implemented".to_string(),
                },
            )
            .await?;
        }
        ClientMessage::Ping => {
            send_message(sender, ServerMessage::Pong).await?;
        }
    }
    Ok(())
}

/// Handle a chat message from the client
async fn handle_chat_message(
    content: String,
    sender: &mut futures::stream::SplitSink<WebSocket, Message>,
) -> Result<()> {
    info!("Processing chat message: {}", content);

    // TODO: Create AgentBridge and send message through it
    // For now, send a simple echo response
    send_message(
        sender,
        ServerMessage::TextChunk {
            content: format!("Echo: {} (Agent integration pending)", content),
        },
    )
    .await?;

    send_message(sender, ServerMessage::ResponseComplete).await?;

    Ok(())
}

/// Handle getting thread history
async fn handle_get_history(
    sender: &mut futures::stream::SplitSink<WebSocket, Message>,
) -> Result<()> {
    // TODO: Implement history retrieval via agent bridge
    send_message(
        sender,
        ServerMessage::Error {
            message: "History retrieval not yet implemented".to_string(),
        },
    )
    .await?;
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
    fn test_server_message_serialization() {
        let msg = ServerMessage::TextChunk {
            content: "Response".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"textChunk\""));
        assert!(json.contains("\"content\":\"Response\""));
    }
}
