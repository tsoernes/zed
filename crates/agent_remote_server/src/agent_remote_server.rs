mod auth;
mod agent_bridge;
mod qr;
mod server;
mod websocket;

pub use auth::{AuthToken, TokenManager};
pub use qr::generate_qr_code;
pub use server::{RemoteAgentServer, ServerConfig, ServerState};

use anyhow::Result;
use gpui::{App, AppContext, Context, Entity, Task};
use std::sync::Arc;

/// Remote agent server that allows mobile devices to connect and interact with the agent
pub struct AgentRemoteServer {
    server: Option<Entity<RemoteAgentServer>>,
    _task: Option<Task<()>>,
}

impl AgentRemoteServer {
    pub fn new() -> Self {
        Self {
            server: None,
            _task: None,
        }
    }

    /// Start the remote agent server
    pub fn start(&mut self, config: ServerConfig, cx: &mut App) -> Result<()> {
        if self.server.is_some() {
            anyhow::bail!("Server already running");
        }

        let server =
            cx.new(|cx: &mut Context<RemoteAgentServer>| RemoteAgentServer::new(config, cx));
        let task = server.update(cx, |server: &mut RemoteAgentServer, cx| server.start(cx));

        self.server = Some(server);
        self._task = Some(task);

        Ok(())
    }

    /// Stop the remote agent server
    pub fn stop(&mut self, cx: &mut App) {
        if let Some(server) = self.server.take() {
            server.update(cx, |server: &mut RemoteAgentServer, _cx| {
                server.stop();
            });
        }
        self._task = None;
    }

    /// Check if server is running
    pub fn is_running(&self) -> bool {
        self.server.is_some()
    }

    /// Get current server state (address, token, etc.)
    pub fn state(&self, cx: &App) -> Option<Arc<ServerState>> {
        self.server
            .as_ref()
            .and_then(|server: &Entity<RemoteAgentServer>| server.read(cx).state())
    }

    /// Get QR code for pairing (PNG bytes)
    pub fn qr_code(&self, cx: &App) -> Option<Vec<u8>> {
        self.state(cx)
            .and_then(|state| generate_qr_code(&state.pairing_url).ok())
    }
}

impl Default for AgentRemoteServer {
    fn default() -> Self {
        Self::new()
    }
}
