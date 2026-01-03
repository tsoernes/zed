# Agent Integration Challenges and Solutions

## Current State

**Branch:** android-agent-chat  
**Status:** Phase 2 implementation encountering GPUI async pattern complexity

## Completed Work

### Infrastructure (Phase 1) ✅
- WebSocket server with authentication
- QR code pairing
- Message protocol definitions
- Web UI

### Agent Integration Attempt (Phase 2) 🚧
- Created AgentBridge with channel-based communication
- Implemented ConnectionHandle for multi-client support
- Updated server to pass AgentBridge to WebSocket handlers
- Refactored for proper async/Send requirements

## Current Challenges

### 1. GPUI Async Context Mismatch

**Problem:**  
- `AgentBridge` needs to call `AcpThread.send()` which requires `&mut Context<AcpThread>`
- Spawned tasks receive `AsyncApp` context, not `Context<T>`
- `WeakEntity<T>.update(cx, ...)` in async contexts has different semantics

**Error Examples:**
```
error[E0277]: the trait bound `&mut AsyncApp: AppContext` is not satisfied
```

**Root Cause:**  
GPUI's async model requires careful handling of context types. Spawned tasks can't directly mutate entities the same way synchronous code can.

### 2. Subscription Lifecycle

**Problem:**  
- `Subscription` type is not `Send`
- Can't store subscription in a struct that needs to be `Send + Sync`
- Need subscription to stay alive for event forwarding

**Attempted Solutions:**
- ❌ Store in `Arc<Mutex<Option<Subscription>>>` - still not Send
- ❌ Drop subscription reference - events stop being received  
- ⏳ Keep subscription in spawned task scope - needs testing

### 3. Message Type Compatibility

**Problem:**  
```rust
// This doesn't work:
acp::ContentBlock::Text(acp::TextContent { text: content })

// Error: missing fields `annotations` and `meta`
```

**Solution Needed:**  
Look up the actual `TextContent` struct definition and provide all required fields.

### 4. Async Future Handling

**Problem:**  
```rust
.and_then(|future| async move { future.await }.now_or_never())
```

`now_or_never()` doesn't exist on async blocks. Need proper await handling in async context.

## Architectural Options

### Option A: Channel-Based Bridge (Current Attempt)

**Pros:**
- Clean separation between tokio and GPUI threads
- Multiple connections can share one bridge
- Type-safe message passing

**Cons:**
- Complex GPUI async patterns
- Hard to get context types right
- Subscription lifecycle tricky

**Status:** Partially implemented, compilation errors

### Option B: GPUI Task-Based

**Approach:**
```rust
// In WebSocket handler (tokio thread)
let (tx, rx) = oneshot::channel();
cx.spawn(|thread, cx| async move {
    let result = thread.update(cx, |t, cx| {
        t.send(message, cx)
    })?;
    tx.send(result.await).ok();
}).detach();

let response = rx.await?;
```

**Pros:**
- Follows GPUI patterns more closely
- Clear context ownership
- Easier to reason about lifetimes

**Cons:**
- One task per message (might be overhead)
- Harder to broadcast to multiple clients
- Need channel for every operation

### Option C: Polling Model

**Approach:**
- WebSocket handler stores messages in a queue
- Separate GPUI entity polls queue periodically
- Entity calls agent methods on GPUI thread
- Entity pushes responses back via channel

**Pros:**
- All agent interaction on GPUI thread
- Simpler async handling
- Clear separation of concerns

**Cons:**
- Polling overhead
- Latency from polling interval
- More moving parts

### Option D: Event-Based Push

**Approach:**
- WebSocket creates weak entity handle to agent thread
- On message, sends GPUI action/event
- Agent thread handles action, emits response events
- Bridge observes events, forwards to WebSocket

**Pros:**
- Idiomatic GPUI pattern
- No context mismatch
- Natural event flow

**Cons:**
- Need to define actions for all message types
- Action dispatch overhead
- More GPUI-specific code

## Recommended Path Forward

### Short-term: Get It Working

**Use Option B** (Task-Based) for immediate functionality:

1. **Remove AgentBridge's agent communication logic**
   - Keep only the client broadcasting functionality
   - Make it a simple pub/sub for WebSocket clients

2. **Handle agent communication in WebSocket handler**
   ```rust
   async fn handle_websocket(socket: WebSocket, thread: WeakEntity<AcpThread>, cx: AsyncApp) {
       // ...
       ClientMessage::Chat { content } => {
           let (tx, rx) = oneshot::channel();
           cx.spawn(|thread, mut cx| async move {
               let future = thread.update(&mut cx, |t, cx| {
                   let blocks = vec![/* create blocks properly */];
                   t.send(blocks, cx)
               })?;
               tx.send(future.await).ok();
           }).detach();
           
           // Wait for response or timeout
           match tokio::time::timeout(Duration::from_secs(30), rx).await {
               Ok(Ok(Ok(_))) => {
                   // Success - responses will come via events
               }
               _ => {
                   send_error("Failed to send message");
               }
           }
       }
   }
   ```

3. **Subscribe to thread events separately**
   - Create subscription when WebSocket connects
   - Keep subscription in a GPUI-managed structure
   - Forward events to WebSocket via channel

### Long-term: Proper Architecture

Once basic functionality works:

1. **Design proper GPUI integration patterns**
2. **Consider Option D** (Event-Based) for cleaner code
3. **Add proper error handling and retries**
4. **Implement streaming response handling**
5. **Add comprehensive tests**

## Immediate Next Steps

1. **Simplify AgentBridge**
   - Remove direct agent communication
   - Focus on client broadcast only
   - Make it just a pub/sub system

2. **Move agent communication to server**
   - Server creates weak entity handle
   - Server spawns tasks to call agent methods
   - Server subscribes to events

3. **Fix compilation errors**
   - Remove AsyncApp context misuse
   - Properly create ContentBlock with all fields
   - Handle async properly without `now_or_never()`

4. **Test basic flow**
   - Send message from mobile
   - Verify it reaches agent
   - See response come back

5. **Iterate and improve**
   - Add proper event streaming
   - Handle tool execution
   - Support multiple clients

## References

- `crates/acp_thread/src/acp_thread.rs` - Agent thread implementation
- `crates/agent_ui/src/acp/thread_view.rs` - UI integration example
- GPUI spawn patterns throughout codebase

## Conclusion

The current approach is close but needs simplification to match GPUI's async patterns. The recommended path is to:
1. Simplify the bridge to just handle client broadcasting
2. Move agent communication logic closer to GPUI context
3. Use established patterns from the codebase

Estimated time to working prototype: 4-6 hours with simplified approach.
