# Chat History Documentation

This directory contains comprehensive documentation for the `ctx_chat_history` tool and its investigation/implementation.

## Documents Overview

### 📘 User Documentation

#### [TOOL_USAGE_GUIDE.md](TOOL_USAGE_GUIDE.md)
**Complete user reference guide (620 lines)**

- Overview of all 12 operations
- 30+ usage examples with expected responses
- Parameter documentation for each operation
- Common use case patterns
- Error handling guide
- Performance tips and best practices
- Backend initialization requirements

**Target Audience**: AI assistants, developers using the tool, users wanting to understand capabilities

**Quick Start**: Read this first to understand how to use the tool.

---

### 🔧 Technical Documentation

#### [IMPROVEMENTS_SUMMARY.md](IMPROVEMENTS_SUMMARY.md)
**Technical summary of changes (211 lines)**

- Problem statement and objectives
- Detailed list of changes made
- Before/after comparisons
- Testing results
- Backward compatibility notes
- Impact assessment

**Target Audience**: Developers, code reviewers, maintainers

---

#### [ENUM_DESERIALIZATION_INVESTIGATION.md](ENUM_DESERIALIZATION_INVESTIGATION.md)
**Root cause analysis (331 lines)**

- Architecture overview of tool invocation flow
- Investigation findings and key discoveries
- Root cause: String-wrapped JSON parameters
- Comparison of LLM vs MCP invocation paths
- Proposed solutions with pros/cons
- Recommended action plan
- Open questions for future work

**Target Audience**: Developers investigating protocol issues, maintainers planning fixes

**Key Finding**: MCP invocations wrap JSON parameters as strings, while LLM-generated calls work correctly.

---

### ✅ Testing Documentation

#### [TESTING_CUSTOM_DESERIALIZER.md](TESTING_CUSTOM_DESERIALIZER.md)
**Comprehensive test plan (456 lines)**

- 14 test categories covering all operations
- Prerequisites and setup instructions
- Test case definitions with expected results
- Success criteria per test
- Test result template
- Performance and regression tests

**Target Audience**: QA engineers, developers verifying fixes

**Usage**: Follow this after rebuilding Zed to verify the custom deserializer works.

---

### 📊 Session Documentation

#### [SESSION_SUMMARY.md](SESSION_SUMMARY.md)
**Complete session summary (431 lines)**

- Chronological overview of all work completed
- 7 commits with detailed explanations
- Statistics (2,100+ lines added)
- Key achievements and technical insights
- Current status and next steps
- Lessons learned
- References and file index

**Target Audience**: Project managers, team leads, future developers

**Purpose**: High-level overview of the entire investigation and implementation effort.

---

## Quick Navigation

### I want to...

**Use the tool**
→ Start with [TOOL_USAGE_GUIDE.md](TOOL_USAGE_GUIDE.md)

**Understand what changed**
→ Read [IMPROVEMENTS_SUMMARY.md](IMPROVEMENTS_SUMMARY.md)

**Fix the root cause**
→ Study [ENUM_DESERIALIZATION_INVESTIGATION.md](ENUM_DESERIALIZATION_INVESTIGATION.md)

**Test the implementation**
→ Follow [TESTING_CUSTOM_DESERIALIZER.md](TESTING_CUSTOM_DESERIALIZER.md)

**Get a complete overview**
→ Read [SESSION_SUMMARY.md](SESSION_SUMMARY.md)

---

## Key Concepts

### What is ctx_chat_history?

A tool that provides persistent storage, semantic search, and RAG (Retrieval-Augmented Generation) capabilities for assistant conversations.

**Core Features**:
- 📁 Store and manage chat history with metadata
- 🔍 Hybrid search (BM25 + embeddings)
- 🎯 Find similar conversations
- 💬 RAG-powered question answering
- 🏷️ Tagging and organization
- 📊 Configuration management

### The "Similar" Operation

**Special Feature**: Find chats similar to the current conversation automatically!

```json
{"similar": {"n": 10}}  // No chat_id needed - uses current conversation
```

This was a major improvement making the tool much more intuitive to use.

---

## Technical Architecture

### Tool Structure

```
ChatHistoryOperation (Enum)
├── Similar         - Find related conversations
├── Search          - Keyword/semantic search
├── Answer          - RAG synthesis
├── List            - List stored chats
├── Get             - Retrieve specific chat
├── Append          - Add message
├── CreateChat      - New chat session
├── DeleteChat      - Remove chat
├── UpdateMetadata  - Modify tags/status
├── Reembed         - Recompute embeddings
├── ConfigGet       - View configuration
└── ConfigSet       - Update configuration
```

### Implementation Files

**Core Implementation**:
- `crates/assistant_tools/src/context_management/chat_history_tool.rs` - Operation enum & schema
- `crates/agent2/src/tools/chat_history_tool.rs` - Agent tool integration
- `crates/chat_history_tools/src/lib.rs` - Adapter implementation
- `crates/chat_history/src/chat_history.rs` - Backend logic

**Related**:
- `crates/agent2/src/thread.rs:2936` - Deserialization site
- `crates/language_model/src/language_model.rs` - LanguageModelToolUse definition

---

## Issue Tracking

### Fixed Issues

✅ **Tool not discoverable** - Enhanced schema with comprehensive descriptions
✅ **chat_id required for similar** - Made optional, auto-uses current conversation
✅ **Enum deserialization failure** - Implemented custom deserializer workaround

### Known Issues

⚠️ **Backend not initialized** - Chat history adapter may not be installed (expected)
⚠️ **String wrapping in MCP** - Root cause at protocol layer (workaround in place)
⚠️ **MemoryOperation has same issue** - Needs same custom deserializer treatment

### Future Work

🔮 **Proper protocol fix** - Address parameter encoding at MCP/ACP boundary
🔮 **Backend initialization** - Ensure chat_history adapter starts with Zed
🔮 **Integration tests** - Add tests to prevent regression
🔮 **ANN indexing** - HNSW for large corpus (when needed)

---

## Statistics

### Documentation Created
- **Total Lines**: ~2,100 lines
- **Documents**: 5 comprehensive guides
- **Examples**: 30+ usage examples
- **Test Cases**: 14 test categories

### Code Changes
- **Files Modified**: 2 core implementation files
- **Lines Added**: ~220 lines (custom deserializers)
- **Helper Struct**: 140+ lines (avoid recursion)
- **Commits**: 7 focused commits

---

## Contributing

When working on chat history tools:

1. **Read the documentation** - Start with TOOL_USAGE_GUIDE.md
2. **Understand the investigation** - Review ENUM_DESERIALIZATION_INVESTIGATION.md
3. **Follow the test plan** - Use TESTING_CUSTOM_DESERIALIZER.md
4. **Update documentation** - Keep these files current with changes

### Adding New Operations

1. Add variant to `ChatHistoryOperation` enum
2. Add to `ChatHistoryOperationHelper` enum
3. Update `From` trait implementation
4. Add handler in agent2 tool
5. Update schema in `input_schema()` method
6. Add documentation to TOOL_USAGE_GUIDE.md
7. Add test case to TESTING_CUSTOM_DESERIALIZER.md

---

## Version History

- **January 15, 2025** - Initial documentation created
  - Enhanced schema and descriptions
  - Made chat_id optional for similar operation
  - Investigated and fixed deserialization issue
  - Created comprehensive documentation suite

---

## Contact & Support

For issues or questions:
- Check existing documentation first
- Review investigation document for known issues
- Check Zed logs: `~/.local/share/zed/logs/Zed.log`
- File issues with reference to investigation findings

---

## License

This documentation is part of the Zed project and follows the project's licensing terms.

---

**Last Updated**: January 15, 2025
**Documentation Version**: 1.0
**Status**: Complete - Ready for testing after rebuild