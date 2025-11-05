# Project Info Tool

## Overview

The `project_info` tool is an internal LLM agent tool that maintains persistent context about a project across chat sessions. It allows the agent to store and retrieve project-specific information in Zed's internal database, which is automatically included in the system prompt for every new conversation in that project.

## Purpose

As the LLM agent works on a project, it learns about:
- Project structure and conventions
- Important file locations
- Key APIs and functions
- Build processes and workflows
- Architectural decisions

The `project_info` tool enables the agent to persist this knowledge so it's available in future conversations, reducing the need to re-discover project details.

## Tool Actions

The tool supports four actions:

### 1. Read
Retrieves the current project info.

```json
{
  "action": "read"
}
```

**Returns:** The stored project info, or a message indicating no info has been stored yet.

### 2. Append
Adds new information to the existing project info.

```json
{
  "action": "append",
  "content": "New information to add..."
}
```

**Returns:** Success message confirming the update.

### 3. Set
Replaces all project info with new content.

```json
{
  "action": "set",
  "content": "Complete new project info..."
}
```

**Returns:** Success message confirming the update.

### 4. Clear
Deletes all project info for the current project.

```json
{
  "action": "clear"
}
```

**Returns:** Success message confirming the deletion.

## How It Works

### Storage
- Project info is stored in a SQLite database table (`project_info`)
- Each project is identified by its worktree root paths (joined with `;`)
- Content is stored as plain text (markdown-friendly)

### Integration with System Prompt
- When a new agent thread is created, project info is loaded asynchronously from the database
- If project info exists, it appears in a "Project Context" section at the beginning of the system prompt
- The agent is encouraged in the system prompt to update project info as it learns

### Example System Prompt Section
```markdown
## Project Context

The following information about this project has been stored from previous conversations:

- Main entry point: src/main.rs
- Uses tokio for async runtime
- API endpoints are defined in src/api/routes.rs
- Database schema is in migrations/
```

## Best Practices for Agents

When working with projects, consider updating project info when you:

1. **Discover Key Files**: Note important file locations and their purposes
   ```
   Append: "Core business logic is in src/services/. Each service handles a specific domain."
   ```

2. **Learn Patterns**: Document project-specific patterns or conventions
   ```
   Append: "Error handling follows Result<T, AppError> pattern. AppError is defined in src/errors.rs."
   ```

3. **Understand Architecture**: Record architectural decisions
   ```
   Append: "Uses hexagonal architecture: Domain (core/) → Application (app/) → Infrastructure (infra/)"
   ```

4. **Note Build Processes**: Document how to build or test
   ```
   Append: "Build with: cargo build --release. Tests require Docker for integration tests."
   ```

5. **Track APIs**: Keep track of important APIs and functions
   ```
   Append: "Main API entry: handle_request() in src/lib.rs. Returns RequestResult."
   ```

## Implementation Details

### Files Modified
- `crates/agent2/src/db.rs` - Database schema and operations
- `crates/agent2/src/tools/project_info_tool.rs` - Tool implementation
- `crates/agent2/src/thread.rs` - Thread integration
- `crates/agent2/src/templates.rs` - Template structure
- `assets/prompts/assistant_system_prompt.hbs` - System prompt template

### Database Schema
```sql
CREATE TABLE IF NOT EXISTS project_info (
    project_path TEXT PRIMARY KEY,
    content TEXT NOT NULL,
    updated_at TEXT NOT NULL
)
```

### Testing
Comprehensive tests are included in `project_info_tool.rs` covering:
- Reading empty/populated project info
- Setting and appending content
- Clearing project info
- Error handling

## Future Enhancements

Potential improvements:
1. Version history of project info changes
2. Per-user project info (currently shared across all users)
3. Structured sections (e.g., separate fields for architecture, conventions, etc.)
4. Import/export functionality
5. Project info suggestions based on repository analysis
