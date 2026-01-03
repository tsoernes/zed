# Agent Remote Server

WebSocket-based server enabling remote access to Zed's LLM agent from mobile devices over local network.

## Overview

This crate provides infrastructure for accessing Zed's agent from mobile browsers via WebSocket. Users can scan a QR code to connect their phone/tablet and interact with the agent while away from their computer.

## Status

**Phase 1 - Infrastructure**: ✅ **COMPLETE**
- HTTP/WebSocket server (Axum)
- Cryptographic token authentication
- QR code generation (SVG)
- Mobile-optimized web UI
- JSON-based message protocol
- Health check endpoint
- Multi-client support

**Phase 2 - Agent Integration**: ✅ **COMPLETE** (95%)
- ✅ AgentBridge pub/sub system
- ✅ AgentCoordinator GPUI entity
- ✅ Message routing (client → agent → client)
- ✅ Chat, GetHistory, Cancel message handlers
- ✅ All tests passing (18/18)
- ✅ Clippy passing
- ⏳ Event subscription for real-time streaming
- ⏳ Zed UI integration
- ⏳ End-to-end testing

**Phase 3 - Advanced Features**: 📋 **PLANNED**
- Multi-instance support (select between open Zed instances)
- Enhanced web UI (markdown, syntax highlighting)
- File upload from mobile
- History synchronization
- Push notifications (PWA)
- Tool execution visualization

## Architecture

```
Mobile Browser (WebSocket)
    ↓ JSON messages (tokio thread)
WebSocket Handler
    ↓ mpsc channel
AgentBridge (pub/sub broadcaster)
    ↓ mpsc channel
AgentCoordinator (GPUI entity)
    ↓ AcpThread.send()
Zed Agent (processes message)
    ↓ Future completes
AgentCoordinator
    ↓ mpsc channel
AgentBridge (broadcast to all clients)
    ↓ per-connection channels
WebSocket Handler
    ↓ JSON messages (tokio thread)
Mobile Browser (displays response)
```

## Components

### AgentBridge (`agent_bridge.rs`)
Pure pub/sub system for broadcasting agent messages to multiple WebSocket clients.
- Creates per-connection handles
- Broadcasts messages to all connected clients
- Automatic cleanup on disconnect
- No GPUI dependencies

### AgentCoordinator (`agent_coordinator.rs`)
GPUI entity that owns message channels and coordinates agent communication.
- Spawns tasks with proper GPUI context
- Handles Chat, GetHistory, Cancel messages
- Forwards to AcpThread using proper async patterns
- Emits coordinator events

### Server (`server.rs`)
Axum-based HTTP and WebSocket server.
- Auto-selects available port or uses configured port
- Binds to `0.0.0.0` for LAN access
- Creates coordinator and bridge on startup
- Serves web UI and handles upgrades

### WebSocket Handler (`websocket.rs`)
Manages individual WebSocket connections.
- Token-based authentication
- Creates connection handles
- Spawns task to forward agent messages
- Handles incoming client messages

### Auth (`auth.rs`)
Cryptographically secure token authentication.
- 32-character alphanumeric tokens
- Constant-time validation (prevents timing attacks)
- Token regeneration support

### QR Code (`qr.rs`)
Generates QR codes for easy mobile pairing.
- SVG format (lightweight, scalable)
- Encodes WebSocket URL with token

## Usage

### Basic Example

```rust
use agent_remote_server::{AgentRemoteServer, ServerConfig};
use acp_thread::AcpThread;
use gpui::WeakEntity;

// Assuming you have an AcpThread entity
let acp_thread: WeakEntity<AcpThread> = /* ... */;

let config = ServerConfig {
    port: 0, // Auto-select available port
    bind_all_interfaces: true, // Allow LAN access
    acp_thread,
};

let mut server = AgentRemoteServer::new();
server.start(config, cx)?;

// Get pairing information
if let Some(state) = server.state(cx) {
    println!("Pairing URL: {}", state.pairing_url);
    println!("Server running on: {}", state.local_addr);
    
    // Generate QR code
    if let Some(qr_bytes) = server.qr_code(cx) {
        // Display QR code to user
    }
}

// Stop server when done
server.stop(cx);
```

### Integration with Zed UI

```rust
// In agent panel or workspace
use zed_actions::agent::{StartRemoteServer, StopRemoteServer};

// Action handler
fn start_remote_server(&mut self, _: &StartRemoteServer, cx: &mut Context<Self>) {
    let config = ServerConfig {
        port: 8080,
        bind_all_interfaces: true,
        acp_thread: self.thread.downgrade(),
    };
    
    self.remote_server.start(config, cx).ok();
    
    // Show QR code modal with pairing URL
    if let Some(state) = self.remote_server.state(cx) {
        self.show_qr_modal(&state.pairing_url, cx);
    }
}
```

## Message Protocol

### Client → Server Messages

```json
{"type": "chat", "content": "Hello, agent!"}
{"type": "getHistory"}
{"type": "cancel"}
{"type": "ping"}
```

### Server → Client Messages

```json
{"type": "connected", "session_id": "uuid-here"}
{"type": "textChunk", "content": "Response text..."}
{"type": "toolStart", "tool_name": "read_file", "tool_input": {...}}
{"type": "toolResult", "tool_name": "read_file", "result": "...", "error": null}
{"type": "responseComplete"}
{"type": "error", "message": "Error description"}
{"type": "historyEntry", "role": "user", "content": "..."}
{"type": "pong"}
```

## Security

- **Token Authentication**: All connections require valid token
- **Constant-Time Validation**: Prevents timing attacks
- **Local Network Only**: No internet exposure by default
- **Session Isolation**: Each connection gets unique ID
- **No Port Forwarding**: Works entirely on LAN

## Testing

```bash
# Run all tests
cargo test -p agent_remote_server

# Run with clippy
./script/clippy -p agent_remote_server

# Build release version
cargo build -p agent_remote_server --release
```

## Development

### Project Structure

```
agent_remote_server/
├── Cargo.toml
├── README.md
└── src/
    ├── agent_remote_server.rs  # Public API
    ├── agent_bridge.rs         # Pub/sub broadcaster (185 lines)
    ├── agent_coordinator.rs    # GPUI entity (228 lines)
    ├── auth.rs                 # Token auth (129 lines)
    ├── qr.rs                   # QR generation (43 lines)
    ├── server.rs               # HTTP/WS server (230 lines)
    ├── websocket.rs            # WS handler (290 lines)
    └── web_ui/
        └── index.html          # Mobile UI
```

### Key Dependencies

- `axum` - Web framework
- `tokio-tungstenite` - WebSocket support
- `tower` - Middleware
- `qrcode` - QR code generation
- `acp_thread` - Agent thread integration
- `agent-client-protocol` - Message types
- `gpui` - UI framework and async patterns

## How It Works

1. **Server Start**
   - Zed creates `AgentRemoteServer` with `ServerConfig`
   - Server creates `AgentBridge` and `AgentCoordinator`
   - Coordinator spawns GPUI task to process messages
   - Server binds to local port (e.g., `192.168.1.100:8080`)
   - Generates authentication token and QR code

2. **Pairing**
   - User scans QR code with phone camera
   - Opens WebSocket URL: `ws://192.168.1.100:8080?token=abc123...`
   - Server validates token
   - WebSocket connection established
   - Connection handle created

3. **Chat Flow**
   - User types message in mobile UI
   - WebSocket sends `{"type":"chat","content":"..."}`
   - Server forwards to AgentBridge
   - Bridge sends to AgentCoordinator via channel
   - Coordinator updates AcpThread on GPUI thread
   - AcpThread.send() processes message with agent
   - Agent responses flow back through same path in reverse
   - All connected clients receive broadcasts

4. **Multi-Client Support**
   - Each connection gets unique `ConnectionHandle`
   - Bridge maintains map of active connections
   - Agent responses broadcast to all clients
   - Disconnected clients automatically removed

## Future Enhancements

### Multi-Instance Support
When multiple Zed instances are running, allow users to:
- Discover all instances via mDNS/Bonjour
- Select which instance to connect to
- Switch between instances in web UI

**Proposed Implementation:**
- Each Zed instance runs server on different port
- mDNS advertisement with instance metadata
- Web UI shows list of discovered instances
- Or: Single server proxies to multiple agents

### Enhanced Web UI
- Markdown rendering for agent responses
- Syntax highlighting for code blocks
- Tool execution progress visualization
- File preview and download
- Image support
- PWA with offline capability

### Advanced Features
- File upload from mobile (drag & drop)
- Push notifications for long-running operations
- Voice input support
- Camera integration for image analysis
- Persistence across reconnections

## Troubleshooting

### Connection Issues

**Problem**: Can't connect from mobile device

**Solutions**:
1. Verify devices are on same network
2. Check firewall allows port (default: auto-selected)
3. Ensure `bind_all_interfaces: true` in config
4. Try different port if auto-select fails

### Token Issues

**Problem**: "Invalid authentication token" error

**Solutions**:
1. Scan QR code again for fresh token
2. Check URL includes `?token=...` parameter
3. Verify token wasn't truncated when copying

### Agent Not Responding

**Problem**: Messages sent but no response

**Solutions**:
1. Verify AcpThread is active
2. Check Zed logs for errors
3. Ensure agent has active session
4. Try cancelling and resending

## Contributing

This crate follows Zed's development guidelines:
- Use `./script/clippy` instead of `cargo clippy`
- Follow GPUI patterns for async code
- Add tests for new functionality
- Update this README with changes

## License

GPL-3.0-or-later

## Related Documentation

- `crates/acp_thread/` - Agent thread implementation
- `crates/agent_ui/` - Agent UI components
- GPUI async patterns in codebase