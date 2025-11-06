# Testing Custom Deserializer Fix

## Overview

After implementing the custom deserializer for `ChatHistoryOperation`, this document provides a comprehensive test plan to verify the fix works correctly.

## Prerequisites

1. **Rebuild Zed Editor**
   ```bash
   cd zed
   cargo build --release
   ```

2. **Restart Zed**
   - Close all Zed instances
   - Launch the newly built version
   - Ensure agent2 connection is active

## Test Categories

### Category 1: Unit-like Struct Variants (Simplest)

#### Test 1.1: ConfigGet
**Operation**: Get current configuration

**MCP Invocation Format**:
```json
{
  "operation": {
    "type": "config_get"
  }
}
```

**Expected Result**: Configuration object with settings (secrets redacted)

**Success Criteria**:
- ✅ No deserialization errors
- ✅ Returns valid JSON configuration
- ✅ Contains expected fields (embedding_model, hybrid_alpha, etc.)

---

### Category 2: Simple Struct Variants (Optional Fields)

#### Test 2.1: List Chats
**Operation**: List stored chats with pagination

**MCP Invocation Format**:
```json
{
  "operation": {
    "type": "list",
    "limit": 10,
    "offset": 0
  }
}
```

**Expected Result**: Array of chat metadata

**Success Criteria**:
- ✅ No deserialization errors
- ✅ Returns list of chats (may be empty if none exist)
- ✅ Respects limit parameter

#### Test 2.2: List (No Parameters)
**Operation**: List with all defaults

**MCP Invocation Format**:
```json
{
  "operation": {
    "type": "list"
  }
}
```

**Expected Result**: Default list (20 chats)

**Success Criteria**:
- ✅ No deserialization errors
- ✅ Uses default limit

---

### Category 3: Similar Operation (Key Feature)

#### Test 3.1: Similar Without chat_id
**Operation**: Find chats similar to current conversation

**MCP Invocation Format**:
```json
{
  "operation": {
    "type": "similar",
    "n": 5,
    "project_scoped": true
  }
}
```

**Expected Result**: List of similar chats (may require active thread_id)

**Success Criteria**:
- ✅ No deserialization errors
- ✅ Uses current conversation's thread_id
- ✅ Returns similarity scores

#### Test 3.2: Similar With chat_id
**Operation**: Find chats similar to specific chat

**MCP Invocation Format**:
```json
{
  "operation": {
    "type": "similar",
    "chat_id": "some-chat-id-123",
    "n": 10
  }
}
```

**Expected Result**: Similar chats or error if chat_id doesn't exist

**Success Criteria**:
- ✅ No deserialization errors
- ✅ Uses specified chat_id
- ✅ Returns appropriate error if chat not found

---

### Category 4: Search Operations (Required Fields)

#### Test 4.1: Search
**Operation**: Keyword/semantic search

**MCP Invocation Format**:
```json
{
  "operation": {
    "type": "search",
    "query": "rust async programming",
    "mode": "hybrid",
    "top_k": 10
  }
}
```

**Expected Result**: Search results with context excerpts

**Success Criteria**:
- ✅ No deserialization errors
- ✅ Required field "query" is validated
- ✅ Returns ranked results

#### Test 4.2: Answer
**Operation**: RAG answer synthesis

**MCP Invocation Format**:
```json
{
  "operation": {
    "type": "answer",
    "question": "How did I implement error handling before?"
  }
}
```

**Expected Result**: Synthesized answer with citations

**Success Criteria**:
- ✅ No deserialization errors
- ✅ Required field "question" is validated
- ✅ Returns answer text

---

### Category 5: Mutation Operations

#### Test 5.1: Create Chat
**Operation**: Create new chat session

**MCP Invocation Format**:
```json
{
  "operation": {
    "type": "create_chat",
    "title": "Test Chat",
    "project_id": "test-project"
  }
}
```

**Expected Result**: New chat metadata with generated chat_id

**Success Criteria**:
- ✅ No deserialization errors
- ✅ Returns new chat with unique ID
- ✅ Title is set correctly

#### Test 5.2: Append Message
**Operation**: Add message to chat

**MCP Invocation Format**:
```json
{
  "operation": {
    "type": "append",
    "content": "This is a test message",
    "role": "User"
  }
}
```

**Expected Result**: Chat and message metadata

**Success Criteria**:
- ✅ No deserialization errors
- ✅ Auto-creates chat if chat_id not provided
- ✅ Returns message with ID

---

### Category 6: Configuration Operations

#### Test 6.1: Config Set
**Operation**: Update configuration

**MCP Invocation Format**:
```json
{
  "operation": {
    "type": "config_set",
    "hybrid_alpha": 0.6,
    "similar_chats_k": 15
  }
}
```

**Expected Result**: Updated configuration (requires confirmation)

**Success Criteria**:
- ✅ No deserialization errors
- ✅ May require user confirmation
- ✅ Returns updated config

---

### Category 7: Complex Operations

#### Test 7.1: Update Metadata
**Operation**: Modify chat metadata

**MCP Invocation Format**:
```json
{
  "operation": {
    "type": "update_metadata",
    "chat_id": "test-chat-123",
    "title": "Updated Title",
    "tags_add": ["important", "follow-up"],
    "archived": false
  }
}
```

**Expected Result**: Updated chat metadata (requires confirmation)

**Success Criteria**:
- ✅ No deserialization errors
- ✅ Required field "chat_id" is validated
- ✅ Partial updates work

---

## Backend Status Tests

### Test 8.1: Backend Not Initialized
**Expected Error**: "chat history adapter not installed"

**This is EXPECTED** until chat_history backend is initialized in Zed startup.

**Success Criteria**:
- ✅ Deserialization succeeds (no JSON parsing errors)
- ✅ Clear error message about adapter
- ✅ Tool execution reaches adapter check

---

## Error Handling Tests

### Test 9.1: Missing Required Field
**MCP Invocation Format**:
```json
{
  "operation": {
    "type": "search"
  }
}
```
**Note**: Missing "query" field

**Expected Result**: Error about missing required field

**Success Criteria**:
- ✅ Deserialization fails gracefully
- ✅ Error message identifies missing field

### Test 9.2: Invalid Field Type
**MCP Invocation Format**:
```json
{
  "operation": {
    "type": "list",
    "limit": "not-a-number"
  }
}
```

**Expected Result**: Type validation error

**Success Criteria**:
- ✅ Deserialization fails gracefully
- ✅ Error message identifies type mismatch

---

## Regression Tests (LLM Flow)

### Test 10.1: LLM-Generated Tool Call
**Scenario**: Let LLM naturally invoke the tool during conversation

**Example Prompt**: "Find conversations similar to this one"

**Expected Result**: LLM generates proper tool call and it executes

**Success Criteria**:
- ✅ LLM can still generate tool calls
- ✅ Tool executes successfully
- ✅ No regression from custom deserializer

---

## Performance Tests

### Test 11.1: Rapid Invocations
**Scenario**: Invoke multiple operations in succession

**Expected Result**: All execute without hanging

**Success Criteria**:
- ✅ No memory leaks
- ✅ Consistent response times
- ✅ No thread blocking

---

## Test Results Template

```markdown
## Test Session: [DATE/TIME]

### Environment
- Zed Version: [commit hash]
- OS: [OS version]
- Agent2 Status: [active/inactive]

### Test Results

| Test ID | Operation | Status | Notes |
|---------|-----------|--------|-------|
| 1.1 | config_get | ✅/❌ | |
| 2.1 | list | ✅/❌ | |
| 2.2 | list (defaults) | ✅/❌ | |
| 3.1 | similar (no id) | ✅/❌ | |
| 3.2 | similar (with id) | ✅/❌ | |
| 4.1 | search | ✅/❌ | |
| 4.2 | answer | ✅/❌ | |
| 5.1 | create_chat | ✅/❌ | |
| 5.2 | append | ✅/❌ | |
| 6.1 | config_set | ✅/❌ | |
| 7.1 | update_metadata | ✅/❌ | |
| 8.1 | backend check | ✅/❌ | |
| 9.1 | missing field | ✅/❌ | |
| 9.2 | invalid type | ✅/❌ | |
| 10.1 | LLM flow | ✅/❌ | |

### Overall Assessment
- Custom Deserializer: [Working/Not Working]
- String Unwrapping: [Working/Not Working]
- LLM Compatibility: [Maintained/Broken]

### Issues Found
1. [Issue description]
2. [Issue description]

### Recommendations
1. [Recommendation]
2. [Recommendation]
```

---

## Success Definition

**The fix is successful if**:
1. ✅ All 12 operation types can be invoked via MCP without deserialization errors
2. ✅ String-wrapped JSON is properly unwrapped
3. ✅ Direct JSON (LLM flow) still works
4. ✅ Error messages are clear and actionable
5. ✅ No performance degradation

**Known Limitation**:
- Backend "not installed" errors are expected until chat_history is initialized

---

## Next Steps After Testing

### If Tests Pass:
1. Apply same pattern to `MemoryOperation` enum
2. Document workaround in codebase
3. Create issue to track proper protocol-layer fix
4. Monitor for edge cases

### If Tests Fail:
1. Check error messages for clues
2. Add debug logging to deserializer
3. Test with simpler JSON structures
4. Consider alternative approaches from investigation doc

---

## Additional Notes

- The custom deserializer is a **workaround**, not a proper fix
- Proper fix requires investigating MCP/ACP parameter encoding
- This solution maintains backward compatibility
- Performance impact should be negligible (one extra parse attempt)

---

## Quick Test Commands

```bash
# Rebuild
cd zed && cargo build --release

# Check logs for errors
tail -f ~/.local/share/zed/logs/Zed.log

# Test in conversation
# Just invoke: ctx_chat_history with various operations
```
