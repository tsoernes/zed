use crate::agent_bridge::AgentBridge;
use crate::agent_coordinator::AgentCoordinator;
use crate::auth::{AuthToken, TokenManager};
use crate::websocket::handle_websocket;
use acp_thread::AcpThread;
use anyhow::Result;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Query, State as AxumState};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use collections::HashMap;
use futures::future::AbortHandle;
use gpui::{AppContext, Context, Entity, Task, WeakEntity};
use log::{error, info};
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
    _agent_bridge: Option<AgentBridge>,
    _coordinator: Option<Entity<AgentCoordinator>>,
    abort_handle: Option<AbortHandle>,
}

impl RemoteAgentServer {
    pub fn new(config: ServerConfig, _cx: &mut Context<Self>) -> Self {
        Self {
            config,
            state: None,
            token_manager: Arc::new(RwLock::new(TokenManager::new())),
            _agent_bridge: None,
            _coordinator: None,
            abort_handle: None,
        }
    }

    /// Start the server
    pub fn start(&mut self, cx: &mut Context<Self>) -> Task<()> {
        let config = self.config.clone();
        let token_manager = Arc::clone(&self.token_manager);

        // Create the agent bridge and coordinator before going async
        let (agent_bridge, to_agent_rx, from_agent_tx) = AgentBridge::new();
        let acp_thread = config.acp_thread.clone();
        let _coordinator =
            cx.new(|cx| AgentCoordinator::new(acp_thread, to_agent_rx, from_agent_tx, cx));

        cx.spawn(async move |this, mut cx| {
            match Self::run_server(config, token_manager.clone(), agent_bridge, &mut cx).await {
                Ok((server_state, abort_handle)) => {
                    info!("Server started on {}", server_state.local_addr);
                    info!("Pairing URL: {}", server_state.pairing_url);

                    // Store state and abort_handle
                    let server_state_arc = Arc::new(server_state);
                    let _ = this.update(cx, |server, _cx| {
                        server.state = Some(server_state_arc);
                        server.abort_handle = Some(abort_handle);
                    });
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
        agent_bridge: AgentBridge,
        _cx: &mut gpui::AsyncApp,
    ) -> Result<(ServerState, AbortHandle)> {
        // Agent bridge and coordinator already created in start()
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
            auth_token,
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
