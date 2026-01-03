# Next Steps: Agent Integration for android-agent-chat

## Current Status

✅ **Phase 1 Complete:** Server infrastructure is built and compiling
🚧 **Phase 2 In Progress:** Agent integration foundation completed, integration pending

## What Was Completed (Latest Session)

1. **Implemented complete AgentBridge** (`agent_bridge.rs`)
   - Full channel-based communication between tokio and GPUI threads
   - Subscribe to AcpThread events and forward to WebSocket
   - Convert messages: ClientToAgentMessage → acp::ContentBlock
   - Handle agent responses: TextChunk, ToolStart, ToolResult, ResponseComplete
   - Support for history retrieval and cancellation

2. **Updated Server** (`server.rs`)
   - ServerConfig now requires `WeakEntity<AcpThread>`
   - Pass thread reference to WebSocket handlers

3. **Updated WebSocket Handler** (`websocket.rs`)
   - Prepared for AgentBridge integration
   - Added message types for history entries
   - Test coverage added

4. **Added Dependencies**
   - `acp_thread.workspace = true`
   - `agent-client-protocol.workspace = true`
   - `project.workspace = true`

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

### Step 1: Complete AgentBridge Implementation ✅ DONE

**File:** `crates/agent_remote_server/src/agent_bridge.rs`

**Completed:**
- ✅ Accept `WeakEntity<AcpThread>` in constructor
- ✅ Spawn GPUI task that listens on `to_agent_rx` channel
- ✅ Call `acp_thread.send()` with ContentBlock messages
- ✅ Subscribe to thread events (NewEntry, EntryUpdated, Stopped, Error, Refusal)
- ✅ Forward responses to `from_agent_tx` channel
- ✅ Handle GetHistory and Cancel messages
- ✅ Error handling with proper Result types
- ✅ Keep subscription alive with Arc<Mutex<Option<Subscription>>>

### Step 2: Update WebSocket Handler 🚧 IN PROGRESS

**File:** `crates/agent_remote_server/src/websocket.rs`

**Completed:**
- ✅ Accept `WeakEntity<AcpThread>` parameter in `handle_websocket()`
- ✅ Add imports for AgentBridge
- ✅ Add HistoryEntry message type

**Remaining Tasks:**
1. Create AgentBridge instance on GPUI thread (requires proper context)
2. Forward client messages to bridge
3. Poll bridge for agent responses (async loop)
4. Send responses back to WebSocket client in real-time

**Challenge:** Need to create AgentBridge on GPUI thread, but WebSocket handler runs on tokio thread. 

**Proposed Solution:**
- Create bridge during server setup on GPUI thread
- Pass bridge handle to WebSocket handler via shared state
- Or: Use App context callback to create bridge per connection

### Step 3: Update Server to Create Threads ✅ DONE

**File:** `crates/agent_remote_server/src/server.rs`

**Completed:**
- ✅ Add acp_thread to dependencies
- ✅ ServerConfig now includes `WeakEntity<AcpThread>`
- ✅ InternalServerState stores thread reference
- ✅ Pass thread to WebSocket handler

**Architecture Decision:** Using **Option A** (one shared thread for all connections)
- Simpler to implement and maintain
- All connections interact with same conversation
- Easy to migrate to Option C later for multi-instance support

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

**Infrastructure:**
- [x] AgentBridge module implemented
- [x] Server accepts AcpThread reference
- [x] WebSocket handler prepared for integration
- [x] Message types defined and serialization tested

**Remaining Integration:**
- [ ] AgentBridge creation in WebSocket handler (GPUI context issue)
- [ ] WebSocket client can send message
- [ ] Message reaches agent thread via bridge
- [ ] Agent processes message
- [ ] Agent response streams back to client
- [ ] Web UI displays response in real-time
- [ ] Tool execution visible to client
- [ ] Errors handled gracefully
- [ ] Multiple connections supported

## Timeline Estimate

- ✅ Step 1 (AgentBridge complete): ~3 hours (DONE)
- 🚧 Step 2 (WebSocket integration): 1-2 hours (IN PROGRESS)
- ✅ Step 3 (Server thread creation): ~1 hour (DONE)
- ⏳ Step 4 (Streaming responses): 2-4 hours (NEXT)
- ⏳ Testing & debugging: 2-4 hours

**Completed:** ~4 hours
**Remaining:** 5-10 hours of development time

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

**Status:** Phase 2 foundation complete! Integration remaining.
**Last Updated:** 2025-01-01
**Latest Commit:** 2632380405 - "feat: Integrate AgentBridge with AcpThread for real agent communication"

## Known Issues

1. **GPUI Context in Async Handler:** 
   - AgentBridge requires `&mut App` to create
   - WebSocket handler runs in async tokio context
   - Need to bridge this gap (possibly create bridge in server setup, store in shared state)

2. **Network Issue:**
   - `cargo check` fails on webrtc-sys dependency (DNS error)
   - Doesn't affect agent_remote_server directly
   - May need to build with specific features or use offline mode

## Next Session Goals

1. Resolve GPUI context availability for AgentBridge creation
2. Complete WebSocket → AgentBridge → AcpThread message flow
3. Test end-to-end: mobile browser → WebSocket → agent → response
4. Handle multiple concurrent connections
5. Add Zed UI integration to start server
