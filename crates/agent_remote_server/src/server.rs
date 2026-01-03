use crate::agent_bridge::{AgentBridge, AgentToClientMessage, ClientToAgentMessage};
use crate::auth::{AuthToken, TokenManager};
use crate::websocket::handle_websocket;
use acp_thread::AcpThread;
use agent_client_protocol as acp;
use anyhow::Result;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Query, State as AxumState};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use collections::HashMap;
use futures::StreamExt;
use futures::future::AbortHandle;
use gpui::{App, Context, Task, WeakEntity};
use log::{error, info, warn};
use parking_lot::RwLock;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tokio::net::TcpListener;

/// Configuration for the remote agent server
#[derive(Clone)]
pub struct ServerConfig {
    /// Port to bind to (0 = auto-select)
    pub port: u16,
    /// Whether to bind to all interfaces (0.0.0.0) or just localhost
    pub bind_all_interfaces: bool,
    /// Reference to the agent thread to connect to
    pub acp_thread: WeakEntity<AcpThread>,
}

/// Current state of the running server
#[derive(Debug, Clone)]
pub struct ServerState {
    /// Local address the server is bound to
    pub local_addr: SocketAddr,
    /// Authentication token
    pub auth_token: AuthToken,
    /// Full pairing URL (ws://...)
    pub pairing_url: String,
}

/// Internal server state shared across handlers
#[derive(Clone)]
struct InternalServerState {
    token_manager: Arc<RwLock<TokenManager>>,
    agent_bridge: AgentBridge,
}

/// Remote agent server that handles HTTP and WebSocket connections
pub struct RemoteAgentServer {
    config: ServerConfig,
    state: Option<Arc<ServerState>>,
    token_manager: Arc<RwLock<TokenManager>>,
    agent_bridge: Option<AgentBridge>,
    abort_handle: Option<AbortHandle>,
}

impl RemoteAgentServer {
    pub fn new(config: ServerConfig, _cx: &mut Context<Self>) -> Self {
        // Bridge will be created when server starts (needs async context)
        Self {
            config,
            state: None,
            token_manager: Arc::new(RwLock::new(TokenManager::new())),
            agent_bridge: None,
            abort_handle: None,
        }
    }

    /// Start the server
    pub fn start(&mut self, cx: &mut Context<Self>) -> Task<()> {
        let config = self.config.clone();
        let token_manager = Arc::clone(&self.token_manager);

        cx.spawn(async move |_this, mut cx| {
            match Self::run_server(config, token_manager.clone(), &mut cx).await {
                Ok((server_state, _abort_handle)) => {
                    info!("Server started on {}", server_state.local_addr);
                    info!("Pairing URL: {}", server_state.pairing_url);
                    // TODO: Store state and abort_handle in _this
                }
                Err(e) => {
                    error!("Failed to start server: {:?}", e);
                }
            }
        })
    }

    /// Stop the server
    pub fn stop(&mut self) {
        if let Some(handle) = self.abort_handle.take() {
            handle.abort();
            info!("Server stopped");
        }
        self.state = None;
    }

    /// Get current server state
    pub fn state(&self) -> Option<Arc<ServerState>> {
        self.state.clone()
    }

    /// Run the axum server
    async fn run_server(
        config: ServerConfig,
        token_manager: Arc<RwLock<TokenManager>>,
        cx: &mut gpui::AsyncApp,
    ) -> Result<(ServerState, AbortHandle)> {
        // Create the agent bridge
        let (agent_bridge, mut to_agent_rx, from_agent_tx) = AgentBridge::new();

        // Spawn a task to handle agent communication on GPUI executor
        let acp_thread = config.acp_thread.clone();
        cx.spawn(|mut cx| async move {
            info!("Agent communication handler started");

            while let Some(msg) = to_agent_rx.next().await {
                match msg {
                    ClientToAgentMessage::Chat { content } => {
                        info!("Forwarding chat message to agent: {}", content);

                        let from_agent_tx_clone = from_agent_tx.clone();
                        let result = acp_thread.update(cx, |thread, cx| {
                            let blocks = vec![content.into()];
                            let send_future = thread.send(blocks, cx);

                            let from_agent_tx = from_agent_tx_clone.clone();
                            cx.spawn(async move |_thread, _cx| match send_future.await {
                                Ok(_) => {
                                    info!("Message sent to agent successfully");
                                }
                                Err(e) => {
                                    error!("Failed to send message to agent: {:?}", e);
                                    let _ =
                                        from_agent_tx.unbounded_send(AgentToClientMessage::Error {
                                            message: format!("Failed to send message: {}", e),
                                        });
                                }
                            })
                            .detach();
                        });

                        if let Err(e) = result {
                            warn!("Failed to update agent thread: {:?}", e);
                            let _ = from_agent_tx.unbounded_send(AgentToClientMessage::Error {
                                message: format!("Agent thread not available: {}", e),
                            });
                        }
                    }
                    ClientToAgentMessage::GetHistory => {
                        info!("Fetching conversation history");

                        let from_agent_tx_clone = from_agent_tx.clone();
                        let result = acp_thread.read_with(cx, |thread, _cx| {
                            let entries = thread.entries();

                            for entry in entries {
                                let msg = match entry {
                                    acp_thread::AgentThreadEntry::UserMessage(msg) => {
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
                                    acp_thread::AgentThreadEntry::AssistantMessage(_msg) => {
                                        Some(AgentToClientMessage::HistoryEntry {
                                            role: "assistant".to_string(),
                                            content: "Assistant response".to_string(),
                                        })
                                    }
                                    _ => None,
                                };

                                if let Some(msg) = msg {
                                    let _ = from_agent_tx_clone.unbounded_send(msg);
                                }
                            }
                        });

                        if let Err(e) = result {
                            warn!("Failed to read agent history: {:?}", e);
                            let _ = from_agent_tx.unbounded_send(AgentToClientMessage::Error {
                                message: format!("Failed to get history: {}", e),
                            });
                        }
                    }
                    ClientToAgentMessage::Cancel => {
                        info!("Cancelling agent operation");

                        let result = acp_thread.update(cx, |thread, cx| {
                            let cancel_task = thread.cancel(cx);

                            cx.spawn(async move |_thread, _cx| {
                                cancel_task.await;
                                info!("Agent operation cancelled");
                            })
                            .detach();
                        });

                        if let Err(e) = result {
                            warn!("Failed to cancel agent operation: {:?}", e);
                            let _ = from_agent_tx.unbounded_send(AgentToClientMessage::Error {
                                message: format!("Failed to cancel: {}", e),
                            });
                        }
                    }
                }
            }

            info!("Agent communication handler stopped");
        })
        .detach();
        let ip = if config.bind_all_interfaces {
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        } else {
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        };

        let addr = SocketAddr::new(ip, config.port);
        let listener = TcpListener::bind(addr).await?;
        let local_addr = listener.local_addr()?;

        info!("Server binding to {}", local_addr);

        // Build the pairing URL
        let auth_token = token_manager.read().current_token().clone();
        let pairing_url = build_pairing_url(local_addr, &auth_token)?;

        let server_state = ServerState {
            local_addr,
            auth_token: auth_token.clone(),
            pairing_url,
        };

        let internal_state = InternalServerState {
            token_manager,
            agent_bridge,
        };

        // Build the router
        let app = Router::new()
            .route("/", get(index_handler))
            .route("/ws", get(websocket_handler))
            .route("/api/health", get(health_handler))
            .with_state(internal_state);

        // Spawn the server in the background
        let (abort_handle, abort_registration) = AbortHandle::new_pair();
        tokio::spawn(async move {
            let server_future = axum::serve(listener, app).into_future();
            let result = futures::future::Abortable::new(server_future, abort_registration).await;
            match result {
                Ok(Ok(())) => info!("Server completed successfully"),
                Ok(Err(e)) => error!("Server error: {:?}", e),
                Err(_) => info!("Server aborted"),
            }
        });

        Ok((server_state, abort_handle))
    }
}

/// Build the WebSocket pairing URL
fn build_pairing_url(addr: SocketAddr, token: &AuthToken) -> Result<String> {
    // Get local IP address for LAN access
    let ip = if addr.ip().is_unspecified() {
        get_local_ip()?
    } else {
        addr.ip()
    };

    Ok(format!("ws://{}:{}?token={}", ip, addr.port(), token))
}

/// Get the local IP address for LAN access
fn get_local_ip() -> Result<IpAddr> {
    // Try to get the local IP by connecting to a remote address
    // This doesn't actually send data, just determines which interface would be used
    let socket = std::net::UdpSocket::bind("0.0.0.0:0")?;
    socket.connect("8.8.8.8:80")?;
    let local_addr = socket.local_addr()?;
    Ok(local_addr.ip())
}

/// Root handler - serves the web UI
async fn index_handler() -> Html<&'static str> {
    Html(include_str!("web_ui/index.html"))
}

/// WebSocket upgrade handler
async fn websocket_handler(
    ws: WebSocketUpgrade,
    Query(params): Query<HashMap<String, String>>,
    AxumState(state): AxumState<InternalServerState>,
) -> Response {
    // Validate authentication token
    let token = match params.get("token") {
        Some(t) => AuthToken::from_string(t.clone()),
        None => {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                "Missing authentication token",
            )
                .into_response();
        }
    };

    if !state.token_manager.read().validate(&token) {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            "Invalid authentication token",
        )
            .into_response();
    }

    // Upgrade to WebSocket with agent bridge
    ws.on_upgrade(move |socket| handle_websocket(socket, state.agent_bridge))
}

/// Health check endpoint
async fn health_handler() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "service": "agent-remote-server"
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_pairing_url() {
        let addr = "192.168.1.100:8080".parse().unwrap();
        let token = AuthToken::from_string("test123".to_string());
        let url = build_pairing_url(addr, &token).unwrap();
        assert_eq!(url, "ws://192.168.1.100:8080?token=test123");
    }
}
