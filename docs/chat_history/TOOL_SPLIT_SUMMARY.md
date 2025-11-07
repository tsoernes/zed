# Chat History Tool Split - Architecture Decision

## Decision

Replaced the single `ctx_chat_history` tool (with 12 operations via enum) with **6 focused, individual tools**:

1. **`chat_search`** - Search across chat history with hybrid BM25 + semantic search
2. **`chat_similar`** - Find conversations similar to current or specified chat
3. **`chat_answer`** - RAG-based question answering with citations
4. **`chat_list`** - Browse stored chats with metadata and pagination
5. **`chat_get`** - Retrieve specific chat with complete message history
6. **`chat_update`** - Update chat metadata, tags, archive/pin status

## Motivation

### Problems with Monolithic Enum-Based Tool

The original `ctx_chat_history` tool had a complex architecture:

```rust
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct ChatHistoryAgentToolInput {
    pub operation: ChatHistoryOperation,  // Complex enum with 12 variants
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatHistoryOperation {
    Search { query: String, ... },
    Similar { chat_id: Option<String>, ... },
    Answer { question: String, ... },
    List { limit: Option<usize>, ... },
    // ... 8 more variants
}
```

**Issues:**
1. **Enum Deserialization Complexity** - Internal tagging, untagged, or external tagging each have tradeoffs
2. **Schema Generation Mismatch** - OpenAPI 3.0 conversion creates different format than Rust expects
3. **Poor Discoverability** - LLMs see one giant tool instead of focused capabilities
4. **Complex Documentation** - Single description must cover 12 different operations
5. **XML Parameter Serialization** - Nested JSON in XML attributes causes string wrapping issues

### Evidence: Memory Tool Works with GPT-5

The `ctx_memory` tool works correctly with GPT-5 using a simpler enum structure (mostly unit variants). This suggested **avoiding complex enum data structures** in tool parameters.

## New Architecture

### Individual Tool Structure

Each tool has a **flat parameter structure** with no enums:

```rust
// chat_search_tool.rs
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct ChatSearchInput {
    pub query: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub top_k: Option<usize>,
    // ... simple fields only
}

pub struct ChatSearchTool;

impl AgentTool for ChatSearchTool {
    type Input = ChatSearchInput;
    type Output = String;
    
    fn name() -> &'static str {
        "chat_search"
    }
    
    fn description(&self) -> SharedString {
        "Search through your chat history using keywords and semantic similarity. \
         Finds relevant messages across all conversations..."
            .into()
    }
}
```

### Benefits

#### 1. **Simple Deserialization**
- No complex enum variants
- Flat JSON objects only
- No tagging strategy needed
- Works reliably across all LLMs

#### 2. **Better LLM Experience**
- Tools have specific, focused purposes
- Clear names indicate functionality
- Detailed descriptions for each tool
- LLMs can easily choose the right tool

#### 3. **Excellent Documentation**
Each tool includes:
- Comprehensive doc comments on the struct
- Detailed parameter descriptions with examples
- Use case guidance
- Clear explanations of when to use each tool

Example from `chat_similar_tool.rs`:
```rust
/// Find conversations that are semantically similar to the current or a specified chat.
///
/// This tool uses embeddings to discover related conversations based on content similarity,
/// helping you find:
/// - Previous discussions about similar topics
/// - Related problems you've worked on before
/// - Conversations that might contain relevant context
/// - Past solutions to similar challenges
///
/// By default, it finds chats similar to your CURRENT conversation (no chat_id needed).
```

#### 4. **Maintainability**
- Each tool is a separate file
- Clear separation of concerns
- Easy to add/remove/modify individual tools
- No shared enum complexity

#### 5. **Consistency with Memory Tool Pattern**
- Follows the working pattern from `ctx_memory`
- Proven to work with GPT-5 and other models
- Simpler architecture that's easier to reason about

## Tool Details

### Core Discovery Tools

#### chat_search
**Purpose:** Find relevant messages across all chats  
**Key Parameters:** `query`, `mode` (hybrid/bm25/embedding), `top_k`  
**Use Cases:**
- "Find discussions about async error handling"
- "Locate where we talked about database optimization"
- Search for specific code examples

#### chat_similar
**Purpose:** Discover related conversations  
**Key Parameters:** `chat_id` (optional, defaults to current), `n`, `project_scoped`  
**Use Cases:**
- Find chats related to current discussion
- Discover similar problem-solving sessions
- Locate conceptually related conversations

**Highlight:** Auto-uses current conversation if no `chat_id` provided!

#### chat_answer
**Purpose:** RAG-based Q&A with citations  
**Key Parameters:** `question`, `mode`, `top_k`  
**Use Cases:**
- "How did I solve X before?"
- "What approach did we decide on for Y?"
- Synthesize information from multiple past discussions

### Management Tools

#### chat_list
**Purpose:** Browse available chats  
**Key Parameters:** `project_id`, `limit`, `offset`  
**Use Cases:**
- See recent conversations
- Browse chat history by project
- Paginated exploration of all chats

#### chat_get
**Purpose:** Retrieve specific chat details  
**Key Parameters:** `chat_id`  
**Use Cases:**
- Review a specific conversation
- Get complete message history
- Analyze past discussion flow

#### chat_update
**Purpose:** Organize and categorize chats  
**Key Parameters:** `chat_id`, `title`, `summary`, `tags_add`, `tags_remove`, `archived`, `pinned`  
**Use Cases:**
- Add descriptive titles
- Categorize with tags
- Archive completed discussions
- Pin important conversations

## Implementation Notes

### Shared Backend
All tools use the same `chat_history_adapter()` backend:

```rust
let adapter = match chat_history_adapter() {
    Some(a) => a,
    None => return Task::ready(Err(anyhow!(
        "Chat history adapter not installed..."
    )))
};
```

### Consistent Output Format
All tools return markdown with JSON blocks:

```rust
let mut output = String::from("# Chat Search Results\n\n");
output.push_str("```json\n");
output.push_str(&serde_json::to_string_pretty(&value)?);
output.push_str("\n```\n");
```

### Error Handling
Clear error messages when adapter not initialized:
```
"Chat history adapter not installed. The chat history system may not be initialized."
```

## Migration Path

### Old Usage (ctx_chat_history)
```json
{
  "operation": {
    "type": "similar",
    "n": 5
  }
}
```

### New Usage (chat_similar)
```json
{
  "n": 5
}
```

Much simpler! No wrapper object, no discriminator field.

## Excluded Operations

The following operations from the original tool were **intentionally omitted** as they're rarely needed:

- `CreateChat` - Chats are auto-created when needed
- `DeleteChat` - Dangerous operation, better done manually
- `Reembed` - Administrative operation
- `ConfigGet`/`ConfigSet` - System configuration, not user-facing
- `Append` - Messages added automatically through conversation

These can be added later if there's demand, as separate tools.

## Testing

### Build Verification
```bash
cargo build --package agent2
# Success - all tools compile cleanly
```

### Schema Generation
Each tool generates a simple, flat schema that LLMs can easily understand:

```json
{
  "type": "object",
  "properties": {
    "query": {
      "type": "string",
      "description": "The search query text..."
    },
    "top_k": {
      "type": "integer",
      "description": "Maximum number of results..."
    }
  },
  "required": ["query"]
}
```

No complex discriminator fields, no nested enums.

## Comparison with Original Approach

| Aspect | Monolithic Tool | Split Tools |
|--------|----------------|-------------|
| **Complexity** | High (enum + 12 variants) | Low (6 simple structs) |
| **Deserialization** | Complex (tagging strategy) | Simple (flat objects) |
| **LLM Discoverability** | Poor (1 tool, 12 ops) | Excellent (6 focused tools) |
| **Documentation** | Complex (covers all ops) | Clear (focused per tool) |
| **Maintenance** | Harder (shared enum) | Easier (separate files) |
| **Schema** | Complex (discriminated union) | Simple (flat objects) |
| **Reliability** | Enum issues with some LLMs | Works consistently |

## Lessons Learned

1. **Simplicity Wins** - Flat structures are more reliable than complex enums
2. **LLM UX Matters** - Focused tools are easier for models to use correctly
3. **Follow Working Patterns** - Memory tool's simple approach was proven
4. **Avoid Fighting the System** - Don't fight enum deserialization; avoid it
5. **Documentation is Key** - Good descriptions help LLMs choose the right tool

## Future Considerations

### Potential Additions
- `chat_delete` - If deletion becomes a common need
- `chat_export` - Export chat to markdown/JSON
- `chat_merge` - Combine related chats
- `chat_tag_search` - Search by tags specifically

### Monitoring
Track which tools are most used to validate the split:
- Are all 6 tools being used?
- Are any tools over/under-utilized?
- Do LLMs choose the right tool for the task?

### Performance
- Individual tools may have slightly more overhead (6 registrations vs 1)
- But simplified deserialization may offset this
- Monitor actual performance impact

## Conclusion

Splitting the chat history tool into focused individual tools:
- ✅ Eliminates complex enum deserialization issues
- ✅ Provides better LLM user experience
- ✅ Follows proven patterns (memory tool)
- ✅ Improves maintainability and documentation
- ✅ Works reliably across different language models

This architectural decision prioritizes **simplicity and reliability** over clever abstraction. The result is a more robust and user-friendly chat history system.

## Files Changed

**New Tools:**
- `crates/agent2/src/tools/chat_search_tool.rs` (156 lines)
- `crates/agent2/src/tools/chat_similar_tool.rs` (131 lines)
- `crates/agent2/src/tools/chat_answer_tool.rs` (157 lines)
- `crates/agent2/src/tools/chat_list_tool.rs` (134 lines)
- `crates/agent2/src/tools/chat_get_tool.rs` (115 lines)
- `crates/agent2/src/tools/chat_update_tool.rs` (168 lines)

**Modified:**
- `crates/agent2/src/tools.rs` - Tool registration
- `crates/agent2/src/thread.rs` - Tool initialization

**Total:** ~891 lines of well-documented, focused tool code

## References

- Original issue: Complex enum deserialization failures
- Working example: `ctx_memory` tool with GPT-5
- Schema investigation: OpenAPI 3.0 enum handling
- Pattern: MCP servers often use separate tools per operation