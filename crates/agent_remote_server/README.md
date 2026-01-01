# Agent Remote Server

WebSocket-based server for remote access to Zed agent from mobile devices.

## Features

- **Local Network Access**: Works on LAN without port forwarding
- **QR Code Pairing**: Easy connection via QR code
- **Token Authentication**: Secure token-based auth
- **Mobile Web UI**: Responsive browser interface
- **WebSocket Communication**: Real-time bidirectional messaging

## Status

**Phase 1 - Infrastructure**: ✅ Complete
- HTTP/WebSocket server (Axum)
- Token authentication
- QR code generation
- Basic web UI
- Message protocol

**Phase 2 - Agent Integration**: 🚧 Planned
- Connect to agent Thread system
- Stream agent responses
- Handle tool execution
- Support multiple connections

**Phase 3 - Advanced Features**: 📋 Future
- Multi-instance support (select between open Zed chats)
- History synchronization  
- File upload
- Enhanced UI with markdown, syntax highlighting

## Usage

```rust
use agent_remote_server::{AgentRemoteServer, ServerConfig};

let config = ServerConfig {
    port: 0, // Auto-select
    bind_all_interfaces: true,
};

let mut server = AgentRemoteServer::new();
server.start(config, cx)?;

// Get pairing info
if let Some(state) = server.state(cx) {
    println!("Pairing URL: {}", state.pairing_url);
}
```

## License

GPL-3.0-or-later
