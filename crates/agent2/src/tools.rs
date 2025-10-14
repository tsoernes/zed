mod context_server_registry;
mod copy_path_tool;
mod create_directory_tool;
mod delete_path_tool;
mod diagnostics_tool;
mod edit_file_tool;
mod enhanced_terminal_tool;
mod fetch_tool;
mod find_path_tool;
mod grep_tool;
mod list_directory_tool;
mod list_history_tool;
mod memory_tool;
<<<<<<< HEAD
mod shell_detector_tool;
=======
>>>>>>> 866b8e5d35 (feat(token_usage_tool): add token usage reporting tool with per-message and memory breakdown options)
mod token_usage_tool;

mod move_path_tool;
mod now_tool;
mod open_tool;
mod read_file_tool;
mod terminal_tool;
mod thinking_tool;
mod web_search_tool;

/// A list of all built in tool names, for use in deduplicating MCP tool names
pub fn default_tool_names() -> impl Iterator<Item = &'static str> {
    [
        CopyPathTool::name(),
        CreateDirectoryTool::name(),
        DeletePathTool::name(),
        DiagnosticsTool::name(),
        EditFileTool::name(),
        FetchTool::name(),
        FindPathTool::name(),
        GrepTool::name(),
        ListDirectoryTool::name(),
        ListHistoryTool::name(),
        MemoryAgentTool::name(),
        TokenUsageTool::name(),
        EnhancedTerminalTool::name(),
        ShellDetectorTool::name(),
        MovePathTool::name(),
        NowTool::name(),
        OpenTool::name(),
        ReadFileTool::name(),
        TerminalTool::name(),
        ThinkingTool::name(),
        WebSearchTool::name(),
    ]
    .into_iter()
}

pub use context_server_registry::*;
pub use copy_path_tool::*;
pub use create_directory_tool::*;
pub use delete_path_tool::*;
pub use diagnostics_tool::*;
pub use edit_file_tool::*;
pub use enhanced_terminal_tool::*;
pub use fetch_tool::*;
pub use find_path_tool::*;
pub use grep_tool::*;
pub use list_directory_tool::*;
pub use list_history_tool::*;
pub use memory_tool::*;
<<<<<<< HEAD
pub use shell_detector_tool::*;
=======
>>>>>>> 866b8e5d35 (feat(token_usage_tool): add token usage reporting tool with per-message and memory breakdown options)
pub use token_usage_tool::*;

pub use move_path_tool::*;
pub use now_tool::*;
pub use open_tool::*;
pub use read_file_tool::*;
pub use terminal_tool::*;
pub use thinking_tool::*;
pub use web_search_tool::*;

use crate::AgentTool;
