# Agent Remote Server

WebSocket-based server enabling remote access to Zed's LLM agent from mobile devices over local network or internet (via cloudflared).

## Overview

This crate provides infrastructure for accessing Zed's agent from mobile browsers via WebSocket. Users can scan a QR code to connect their phone/tablet and interact with the agent while away from their computer.

The server supports two modes of operation:
1. **Local Network Mode** - Server binds to LAN IP, accessible only on the same network
2. **Internet Mode** - Server binds to localhost and uses a cloudflared tunnel for internet access

## Status

**Phase 1 - Infrastructure**: ✅ **COMPLETE**
- HTTP/WebSocket server (Axum)
- Cryptographic token authentication
- QR code generation (SVG)
- Mobile-optimized web UI
- JSON-based message protocol
- Health check endpoint
- Multi-client support

**Phase 2 - Agent Integration**: ✅ **COMPLETE**
- ✅ AgentBridge pub/sub system
- ✅ AgentCoordinator GPUI entity
- ✅ Message routing (client → agent → client)
- ✅ Chat, GetHistory, Cancel message handlers
- ✅ All tests passing (18/18)
- ✅ Clippy passing
- ✅ Zed UI integration
- ✅ Auto-show modal when server ready
- ⏳ Event subscription for real-time streaming
- ⏳ End-to-end testing

**Phase 3 - Internet Access**: ✅ **COMPLETE**
- ✅ Cloudflared tunnel integration
- ✅ Auto-installation support (Linux/macOS/Windows)
- ✅ Two server modes (Local/Internet)
- ✅ Public URL generation and polling
- ✅ Mode-specific UI indicators

**Phase 4 - Advanced Features**: 📋 **PLANNED**
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
Axum-based HTTP and WebSocket server with dual-mode support.
- Auto-selects available port or uses configured port
- **Local Mode**: Binds to `0.0.0.0` for LAN access
- **Internet Mode**: Binds to `127.0.0.1` and uses cloudflared tunnel
- Creates coordinator and bridge on startup
- Serves web UI and handles upgrades
- Polls for public URL in internet mode

### Cloudflared (`cloudflared.rs`)
Manages cloudflared tunnel lifecycle for internet access.
- Auto-detects cloudflared installation
- Auto-installs cloudflared if not present (Linux/macOS/Windows)
- Starts and manages tunnel process
- Extracts public URL from tunnel output
- Provides non-blocking URL polling

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
    mode: ServerMode::Local, // Local network access
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

### Internet Mode Example

```rust
use agent_remote_server::{AgentRemoteServer, ServerConfig, ServerMode};

// Start server with cloudflared tunnel
let config = ServerConfig {
    port: 0, // Auto-select available port
    mode: ServerMode::Internet, // Internet access via cloudflared
    acp_thread,
};

let mut server = AgentRemoteServer::new();
server.start(config, cx)?;

// Wait for tunnel to establish and get public URL
// The URL will be available after ~5-10 seconds
if let Some(state) = server.state(cx) {
    if let Some(public_url) = &state.public_url {
        println!("Public URL: {}", public_url);
        println!("Share this URL to connect from anywhere!");
    }
}
```

### Integration with Zed UI

```rust
// In agent panel or workspace
use zed_actions::agent::{StartRemoteServer, StopRemoteServer};

// Action handlers for local and internet modes
fn start_remote_server(&mut self, internet_mode: bool, cx: &mut Context<Self>) {
    let mode = if internet_mode {
        ServerMode::Internet
    } else {
        ServerMode::Local
    };
    
    let config = ServerConfig {
        port: 0,
        mode,
        acp_thread: self.thread.downgrade(),
    };
    
    self.remote_server.start(config, cx).ok();
    
    // Modal will auto-show when server is ready
    // For internet mode, waits for public URL
    // For local mode, shows after 500ms
}
```

### Available Actions

Two separate commands for starting the server:

- `agent::StartRemoteServer` - Starts server in local network mode
- `agent::StartRemoteServerInternet` - Starts server in internet mode via cloudflared
- `agent::StopRemoteServer` - Stops the server
- `agent::ShowRemoteServerInfo` - Shows connection information modal

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
- **Local Network Only** (Local Mode): No internet exposure
- **Internet Mode**: Temporary cloudflared tunnels with random URLs
- **Session Isolation**: Each connection gets unique ID
- **Automatic Cleanup**: Tunnels close when server stops

### Internet Mode Considerations

- Cloudflared tunnels are temporary and use random subdomains
- Each session gets a unique `trycloudflare.com` URL
- Authentication token still required for all connections
- Tunnel automatically closes when server stops
- No persistent exposure after server shutdown

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
    ├── cloudflared.rs          # Cloudflared tunnel manager (489 lines)
    ├── qr.rs                   # QR generation (43 lines)
    ├── server.rs               # HTTP/WS server (dual-mode, 280 lines)
    ├── websocket.rs            # WS handler (290 lines)
    └── web_ui/
        └── index.html          # Mobile UI
```

### Key Dependencies

- `axum` - Web framework
- `tokio-tungstenite` - WebSocket support
- `tower` - Middleware
- `qrcode` - QR code generation
- `regex` - URL parsing from cloudflared output
- `acp_thread` - Agent thread integration
- `agent-client-protocol` - Message types
- `gpui` - UI framework and async patterns
- `gpui_tokio` - Bridge between GPUI and Tokio runtimes

### External Dependencies

- `cloudflared` binary (optional, auto-installed for internet mode)

## How It Works

1. **Server Start**
   - Zed creates `AgentRemoteServer` with `ServerConfig` (mode: Local or Internet)
   - Server creates `AgentBridge` and `AgentCoordinator`
   - Coordinator spawns GPUI task to process messages
   - **Local Mode**: Binds to `0.0.0.0` (e.g., `192.168.1.100:8080`)
   - **Internet Mode**: Binds to `127.0.0.1`, starts cloudflared tunnel
   - Generates authentication token and QR code
   - UI modal auto-shows when ready (waits for public URL in internet mode)

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

5. **Internet Mode Tunnel**
   - Server starts cloudflared: `cloudflared tunnel --url http://localhost:<port>`
   - Cloudflared outputs public URL: `https://random-subdomain.trycloudflare.com`
   - Server parses output and extracts URL
   - UI polls server state for public URL
   - Modal displays public URL when available (~5-10 seconds)
   - Users share public URL to access from anywhere
   - Tunnel closes when server stops

## Cloudflared Installation

When internet mode is started without cloudflared installed, the system attempts automatic installation:

### Linux
- **Fedora/RHEL**: `sudo dnf install cloudflared`
- **Debian/Ubuntu**: Downloads and installs `.deb` package
- **Fallback**: Direct binary download to `/usr/local/bin/cloudflared`

### macOS
- **Homebrew**: `brew install cloudflared`
- **Fallback**: Direct binary download and installation

### Windows
- **Chocolatey**: `choco install cloudflared -y`
- **Scoop**: `scoop install cloudflared`
- **Fallback**: Binary download to Program Files

Manual installation is also supported - just ensure `cloudflared` is in PATH.

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
1. **Local Mode**: Verify devices are on same network
2. **Internet Mode**: Check internet connectivity
3. Check firewall allows port (default: auto-selected)
4. Verify server mode matches intended use case
5. Try different port if auto-select fails

### Cloudflared Issues

**Problem**: Tunnel fails to start or public URL not appearing

**Solutions**:
1. Check if cloudflared is installed: `which cloudflared`
2. Manually install: `brew install cloudflared` (macOS) or equivalent
3. Wait 10-15 seconds for tunnel establishment
4. Check cloudflared logs in Zed console
5. Verify localhost is accessible: `curl http://localhost:<port>/api/health`
6. Check if port is blocked by firewall

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