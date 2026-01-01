# Next Steps: Agent Integration for android-agent-chat

## Current Status

✅ **Phase 1 Complete:** Server infrastructure is built and compiling
🚧 **Phase 2 Started:** Agent integration foundation laid

## What We Just Added

Created `agent_bridge.rs` - A module for bridging WebSocket (tokio) and Agent (GPUI) threads using channels.

## Architecture Overview

```
WebSocket Client (Tokio thread)
    ↓
ClientToAgentMessage (via channel)
    ↓
AgentBridge (GPUI thread)
    ↓
AcpThread::send()
    ↓
Agent processes message
    ↓
AgentToClientMessage (via channel)
    ↓
WebSocket sends to client
```

## Implementation Roadmap

### Step 1: Complete AgentBridge Implementation

**File:** `crates/agent_remote_server/src/agent_bridge.rs`

**Tasks:**
1. Accept an `Entity<AcpThread>` or factory function in constructor
2. Spawn GPUI task that:
   - Listens on `to_agent_rx` channel
   - Calls `acp_thread.send()` with content
   - Observes agent responses
   - Sends responses to `from_agent_tx`
3. Handle agent events/streaming responses
4. Implement proper error handling

**Key Code Pattern:**
```rust
pub fn new(acp_thread: WeakEntity<AcpThread>, cx: AsyncApp) -> Result<Self> {
    let (to_agent_tx, mut to_agent_rx) = mpsc::unbounded();
    let (from_agent_tx, from_agent_rx) = mpsc::unbounded();
    
    cx.spawn(async move |mut cx| {
        while let Some(msg) = to_agent_rx.next().await {
            match msg {
                ClientToAgentMessage::Chat { content } => {
                    // Convert to ContentBlock
                    let blocks = vec![acp::ContentBlock::Text(content)];
                    
                    // Send to agent
                    acp_thread.update(&mut cx, |thread, cx| {
                        thread.send(blocks, cx)
                    }).ok();
                    
                    // TODO: Stream responses back via from_agent_tx
                }
                // Handle other message types
            }
        }
    }).detach();
    
    Ok(Self { to_agent_tx, from_agent_rx })
}
```

### Step 2: Update WebSocket Handler

**File:** `crates/agent_remote_server/src/websocket.rs`

**Tasks:**
1. Accept `AgentBridge` parameter in `handle_websocket()`
2. Forward client messages to bridge
3. Poll bridge for agent responses
4. Send responses back to WebSocket client

**Key Changes:**
```rust
pub async fn handle_websocket(
    socket: WebSocket,
    mut bridge: AgentBridge,  // Add this parameter
) {
    // ... existing code ...
    
    // In message handling:
    ClientMessage::Chat { content } => {
        bridge.send(ClientToAgentMessage::Chat { content })?;
        
        // Poll for responses
        while let Some(agent_msg) = bridge.try_recv() {
            match agent_msg {
                AgentToClientMessage::TextChunk { content } => {
                    send_message(&mut sender, ServerMessage::TextChunk { content }).await?;
                }
                AgentToClientMessage::ResponseComplete => {
                    send_message(&mut sender, ServerMessage::ResponseComplete).await?;
                    break;
                }
                // Handle other message types
            }
        }
    }
}
```

### Step 3: Update Server to Create Threads

**File:** `crates/agent_remote_server/src/server.rs`

**Tasks:**
1. Add dependencies for agent/project creation
2. Create or accept `Entity<Project>` in server config
3. Create `Entity<AcpThread>` for each WebSocket connection
4. Pass thread to WebSocket handler via bridge

**Key Architecture Decision:**
- **Option A:** One shared thread for all connections (simpler)
- **Option B:** One thread per connection (isolated conversations)
- **Option C:** Connection can select from available threads (multi-instance support)

**Recommended:** Start with Option A, migrate to C for multi-instance feature.

### Step 4: Handle Agent Streaming Responses

**Challenge:** Agent responses stream over time via events/observations.

**Solution:** Subscribe to agent thread events and forward to channel.

**Pattern:**
```rust
// In AgentBridge::new()
let subscription = cx.subscribe(&acp_thread, move |_thread, event, cx| {
    match event {
        // AgentThreadEvent::MessageUpdated => ...
        // Forward to from_agent_tx channel
    }
});
```

### Step 5: Multi-Instance Support

**For future Phase 3:**

1. **Discovery Mechanism:**
   - Each Zed instance broadcasts availability via mDNS
   - Or: Single server proxies to multiple agent instances
   - Or: Server lists available threads in current instance

2. **Selection UI:**
   - Web UI fetches list of available instances/threads
   - User selects which to connect to
   - WebSocket includes instance/thread ID in connection

3. **Implementation:**
   ```rust
   // Server maintains map of threads
   struct InternalServerState {
       token_manager: Arc<RwLock<TokenManager>>,
       agent_threads: Arc<RwLock<HashMap<ThreadId, WeakEntity<AcpThread>>>>,
   }
   
   // WebSocket URL includes thread selection
   // ws://192.168.1.100:8080?token=xxx&thread=yyy
   ```

## Required Dependencies

Add to `Cargo.toml`:
```toml
[dependencies]
acp_thread.workspace = true  # For AcpThread
agent_client_protocol.workspace = true  # For ContentBlock
project.workspace = true  # For Project entity
```

## Testing Plan

1. **Unit Tests:**
   - AgentBridge message passing
   - Channel communication
   - Error handling

2. **Integration Tests:**
   - Create mock agent thread
   - Send messages via WebSocket
   - Verify responses

3. **Manual Testing:**
   - Start Zed with project
   - Enable remote server
   - Scan QR with phone
   - Send chat messages
   - Verify agent responses

## Potential Issues & Solutions

### Issue 1: AsyncApp not Send + Sync
**Solution:** Use channels to communicate between tokio and GPUI threads.

### Issue 2: Agent response streaming
**Solution:** Subscribe to thread events, forward to channel, poll in WebSocket handler.

### Issue 3: Thread lifecycle management
**Solution:** Use WeakEntity, handle disconnection gracefully.

### Issue 4: Multiple concurrent clients
**Solution:** Clone/share agent thread, or create one per connection.

## Success Criteria

- [ ] WebSocket client can send message
- [ ] Message reaches agent thread
- [ ] Agent processes message
- [ ] Agent response streams back to client
- [ ] Web UI displays response in real-time
- [ ] Tool execution visible to client
- [ ] Errors handled gracefully
- [ ] Multiple connections supported

## Timeline Estimate

- Step 1 (AgentBridge complete): 2-3 hours
- Step 2 (WebSocket integration): 1-2 hours  
- Step 3 (Server thread creation): 2-3 hours
- Step 4 (Streaming responses): 2-4 hours
- Testing & debugging: 2-4 hours

**Total:** 9-16 hours of development time

## Next Commands to Run

```bash
# After implementing above:
cargo build -p agent_remote_server
cargo test -p agent_remote_server

# Test integration:
cargo run --bin zed
# Enable remote server via UI
# Connect from phone browser
```

## Resources

- `crates/acp_thread/src/acp_thread.rs` - AcpThread::send() method
- `crates/agent_ui/src/acp/thread_view.rs` - Example of using AcpThread
- `crates/agent/src/thread.rs` - Thread internals
- GPUI async patterns in existing crates

---

**Status:** Foundation laid, ready for implementation!
**Last Updated:** 2026-01-01
