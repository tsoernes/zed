use crate::agent_bridge::AgentBridge;
use crate::agent_coordinator::AgentCoordinator;
use crate::auth::{AuthToken, TokenManager};
use crate::cloudflared::CloudflaredTunnel;
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

/// Server mode - local network or internet via cloudflared
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerMode {
    /// Local network access (binds to LAN IP)
    Local,
    /// Internet access via cloudflared tunnel (binds to localhost)
    Internet,
}

/// Configuration for the remote agent server
#[derive(Clone)]
pub struct ServerConfig {
    /// Port to bind to (0 = auto-select)
    pub port: u16,
    /// Server mode (local or internet)
    pub mode: ServerMode,
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
    /// Full pairing URL (http://... for easy mobile access)
    pub pairing_url: String,
    /// Server mode
    pub mode: ServerMode,
    /// Public URL (only for internet mode via cloudflared)
    pub public_url: Option<String>,
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
    tunnel: Option<Arc<CloudflaredTunnel>>,
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
            tunnel: None,
        }
    }

    /// Start the server
    pub fn start(&mut self, cx: &mut Context<Self>) -> Task<()> {
        let config = self.config.clone();
        let token_manager = Arc::clone(&self.token_manager);

        // Create the agent bridge and coordinator before going async
        let (agent_bridge, to_agent_rx, from_agent_tx) = AgentBridge::new(cx);
        let acp_thread = config.acp_thread.clone();
        let coordinator =
            cx.new(|cx| AgentCoordinator::new(acp_thread, to_agent_rx, from_agent_tx, cx));

        // Store the coordinator and bridge to prevent them from being dropped
        self._coordinator = Some(coordinator);
        self._agent_bridge = Some(agent_bridge.clone());

        // Determine the bind address on the UI thread and create a std listener.
        // For internet mode, bind to localhost. For local mode, bind to LAN IP.
        cx.spawn(async move |this, cx| {
            let ip = match config.mode {
                ServerMode::Internet => IpAddr::V4(Ipv4Addr::LOCALHOST),
                ServerMode::Local => {
                    // Bind to all interfaces for LAN access
                    IpAddr::V4(Ipv4Addr::UNSPECIFIED)
                }
            };
            let addr = SocketAddr::new(ip, config.port);

            match std::net::TcpListener::bind(addr) {
                Ok(std_listener) => {
                    if let Err(e) = std_listener.set_nonblocking(true) {
                        error!("Failed to set listener non-blocking: {:?}", e);
                        return;
                    }

                    // Spawn the actual server future on the managed Tokio runtime via gpui_tokio.
                    match gpui_tokio::Tokio::spawn(cx, async move {
                        // Convert the std listener inside Tokio and run the Axum server.
                        Self::run_server_from_std_listener(
                            std_listener,
                            token_manager.clone(),
                            agent_bridge,
                            config.mode,
                        )
                        .await
                    }) {
                        Ok(task) => {
                            match task.await {
                                Ok(Ok((server_state, abort_handle))) => {
                                    info!("Server started on {}", server_state.local_addr);

                                    // For internet mode, start cloudflared tunnel
                                    if config.mode == ServerMode::Internet {
                                        info!("Starting cloudflared tunnel...");

                                        // Start the tunnel
                                        let tunnel = Arc::new(CloudflaredTunnel::new());
                                        let port = server_state.local_addr.port();
                                        let tunnel_clone = tunnel.clone();

                                        // Spawn tunnel start in background using gpui_tokio
                                        gpui_tokio::Tokio::spawn(cx, async move {
                                            match tunnel_clone.start_tunnel(port).await {
                                                Ok(public_url) => {
                                                    info!(
                                                        "Cloudflared tunnel established: {}",
                                                        public_url
                                                    );
                                                }
                                                Err(e) => {
                                                    error!(
                                                        "Failed to start cloudflared tunnel: {:?}",
                                                        e
                                                    );
                                                }
                                            }
                                        })
                                        .ok()
                                        .map(|t| t.detach());

                                        // Store state immediately (public_url will be polled later)
                                        let server_state_arc = Arc::new(server_state);
                                        let _ = this.update(cx, |server, _cx| {
                                            server.state = Some(server_state_arc);
                                            server.abort_handle = Some(abort_handle);
                                            server.tunnel = Some(tunnel);
                                        });
                                    } else {
                                        // Local mode - just store state
                                        info!("Pairing URL: {}", server_state.pairing_url);
                                        let server_state_arc = Arc::new(server_state);
                                        let _ = this.update(cx, |server, _cx| {
                                            server.state = Some(server_state_arc);
                                            server.abort_handle = Some(abort_handle);
                                        });
                                    }
                                }
                                Ok(Err(e)) => {
                                    error!("Failed to start server: {:?}", e);
                                }
                                Err(join_err) => {
                                    error!("Server task join error: {:?}", join_err);
                                }
                            }
                        }
                        Err(e) => {
                            error!("Failed to spawn server task on Tokio runtime: {:?}", e);
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to bind listener on {}: {:?}", addr, e);
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

        // Stop cloudflared tunnel if running
        if let Some(tunnel) = self.tunnel.take() {
            // Use a background thread to stop the tunnel since we don't have async context here
            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async {
                    tunnel.stop().await;
                });
            });
        }

        self.state = None;
    }

    /// Get current server state (with updated public URL if available)
    pub fn state(&self) -> Option<Arc<ServerState>> {
        if let Some(state) = &self.state {
            if state.mode == ServerMode::Internet {
                if let Some(tunnel) = &self.tunnel {
                    // Try to get public URL from tunnel (non-blocking)
                    if let Ok(public_url) = tunnel.try_get_public_url() {
                        if public_url.is_some() && state.public_url != public_url {
                            // Update state with new public URL
                            let mut updated = (**state).clone();
                            updated.public_url = public_url;
                            return Some(Arc::new(updated));
                        }
                    }
                }
            }
            Some(state.clone())
        } else {
            None
        }
    }

    /// Run the axum server from an already-bound std::net::TcpListener.
    ///
    /// This helper is intended to run inside Tokio (the caller will spawn it
    /// via gpui_tokio). It converts the std listener to a Tokio listener and
    /// runs the Axum server on the Tokio reactor.
    async fn run_server_from_std_listener(
        std_listener: std::net::TcpListener,
        token_manager: Arc<RwLock<TokenManager>>,
        agent_bridge: AgentBridge,
        mode: ServerMode,
    ) -> Result<(ServerState, AbortHandle)> {
        // Convert the std listener into a Tokio TcpListener now that we're inside Tokio.
        let listener = tokio::net::TcpListener::from_std(std_listener)?;
        let local_addr = listener.local_addr()?;

        info!("Server binding to {}", local_addr);

        // Build the pairing URL
        let auth_token = token_manager.read().current_token().clone();
        let pairing_url = build_pairing_url(local_addr, &auth_token, mode)?;

        let server_state = ServerState {
            local_addr,
            auth_token,
            pairing_url,
            mode,
            public_url: None, // Will be set later for internet mode
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

        // Spawn the server in the background under Tokio
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

/// Build the HTTP pairing URL (easier for users to open on mobile)
fn build_pairing_url(addr: SocketAddr, token: &AuthToken, mode: ServerMode) -> Result<String> {
    match mode {
        ServerMode::Internet => {
            // For internet mode, the URL will be updated once cloudflared provides the public URL
            // Return a placeholder for now
            Ok(format!("http://localhost:{}?token={}", addr.port(), token))
        }
        ServerMode::Local => {
            // Get local IP address for LAN access
            let ip = if addr.ip().is_unspecified() {
                get_local_ip()?
            } else {
                addr.ip()
            };

            Ok(format!("http://{}:{}?token={}", ip, addr.port(), token))
        }
    }
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
    fn test_build_pairing_url_local() {
        let addr = "192.168.1.100:8080".parse().unwrap();
        let token = AuthToken::from_string("test123".to_string());
        let url = build_pairing_url(addr, &token, ServerMode::Local).unwrap();
        assert_eq!(url, "http://192.168.1.100:8080?token=test123");
    }

    #[test]
    fn test_build_pairing_url_internet() {
        let addr = "127.0.0.1:8080".parse().unwrap();
        let token = AuthToken::from_string("test123".to_string());
        let url = build_pairing_url(addr, &token, ServerMode::Internet).unwrap();
        assert_eq!(url, "http://localhost:8080?token=test123");
    }
}
