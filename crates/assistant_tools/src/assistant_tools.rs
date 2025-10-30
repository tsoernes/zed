mod context_management;
mod copy_path_tool;
mod create_directory_tool;
mod delete_path_tool;
mod detect_binaries_tool;
mod diagnostics_tool;
pub mod edit_agent;
mod edit_file_tool;
mod enhanced_terminal_async_tool;
mod enhanced_terminal_job_status_tool;
mod enhanced_terminal_list_jobs_tool;
mod enhanced_terminal_tool; // EnhancedTerminalTool registered under tool name "enhanced_terminal"
mod fetch_tool;
mod find_path_tool;
mod grep_tool;
mod list_directory_tool;
mod move_path_tool;
mod now_tool;
mod open_tool;
mod project_notifications_tool;
mod read_file_tool;
mod schema;
pub mod templates;
mod terminal_tool;
mod thinking_tool;
mod ui;
mod web_search_tool;

use assistant_tool::ToolRegistry;
use copy_path_tool::CopyPathTool;
use gpui::{App, Entity};
use http_client::HttpClientWithUrl;
use language_model::LanguageModelRegistry;
use move_path_tool::MovePathTool;
use std::sync::Arc;
use web_search_tool::WebSearchTool;

pub(crate) use templates::*;

use crate::create_directory_tool::CreateDirectoryTool;
use crate::delete_path_tool::DeletePathTool;
use crate::diagnostics_tool::DiagnosticsTool;
use crate::edit_file_tool::EditFileTool;

use crate::fetch_tool::FetchTool;
use crate::list_directory_tool::ListDirectoryTool;
use crate::now_tool::NowTool;
use crate::thinking_tool::ThinkingTool;

pub use context_management::{
    ChatHistoryTool, ChatHistoryToolInput, ListHistoryTool, ListHistoryToolInput, MemoryOperation,
    MemoryTool, MemoryToolInput,
};
pub use detect_binaries_tool::DetectBinariesTool;
pub use edit_file_tool::{EditFileMode, EditFileToolInput};
pub use enhanced_terminal_async_tool::EnhancedTerminalAsyncTool;
pub use enhanced_terminal_job_status_tool::EnhancedTerminalJobStatusTool;
pub use enhanced_terminal_list_jobs_tool::EnhancedTerminalListJobsTool;
pub use enhanced_terminal_tool::EnhancedTerminalTool;
pub use find_path_tool::*;
pub use grep_tool::{GrepTool, GrepToolInput};
pub use open_tool::OpenTool;
pub use project_notifications_tool::ProjectNotificationsTool;
pub use read_file_tool::{ReadFileTool, ReadFileToolInput};
pub use terminal_tool::TerminalTool;

pub fn init(http_client: Arc<HttpClientWithUrl>, cx: &mut App) {
    assistant_tool::init(cx);

    let registry = ToolRegistry::global(cx);
    registry.register_tool(TerminalTool);
    registry.register_tool(EnhancedTerminalTool);
    registry.register_tool(EnhancedTerminalAsyncTool);
    registry.register_tool(EnhancedTerminalJobStatusTool);
    registry.register_tool(EnhancedTerminalListJobsTool);
    registry.register_tool(DetectBinariesTool);
    registry.register_tool(CreateDirectoryTool);
    registry.register_tool(CopyPathTool);
    registry.register_tool(DeletePathTool);
    registry.register_tool(MovePathTool);
    registry.register_tool(DiagnosticsTool);
    registry.register_tool(ListDirectoryTool);
    registry.register_tool(NowTool);
    registry.register_tool(OpenTool);
    registry.register_tool(ProjectNotificationsTool);
    registry.register_tool(FindPathTool);
    registry.register_tool(ReadFileTool);
    registry.register_tool(GrepTool);
    registry.register_tool(ThinkingTool);
    registry.register_tool(FetchTool::new(http_client));
    registry.register_tool(EditFileTool);

    // Context management tools
    log::info!("Registering context management tools: list_history, memory, chat_history");
    registry.register_tool(ListHistoryTool);
    registry.register_tool(MemoryTool);
    registry.register_tool(ChatHistoryTool);
    if registry.tools().iter().any(|t| t.name() == "memory") {
        log::info!(
            "assistant_tools registered MemoryTool (native); backend will be noop until thread integration sets a real backend"
        );
    } else {
        log::warn!("MemoryTool missing after registration; memory operations will be unavailable");
    }
    // CallContextTool removed; context tools exposed individually (list_history, memory, chat_history)
    register_web_search_tool(&LanguageModelRegistry::global(cx), cx);
    cx.subscribe(
        &LanguageModelRegistry::global(cx),
        move |registry, event, cx| {
            if let language_model::Event::DefaultModelChanged = event {
                register_web_search_tool(&registry, cx);
            }
        },
    )
    .detach();
}

fn register_web_search_tool(registry: &Entity<LanguageModelRegistry>, cx: &mut App) {
    let using_zed_provider = registry
        .read(cx)
        .default_model()
        .is_some_and(|default| default.is_provided_by_zed());
    if using_zed_provider {
        ToolRegistry::global(cx).register_tool(WebSearchTool);
    } else {
        ToolRegistry::global(cx).unregister_tool(WebSearchTool);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_settings::AgentSettings;
    use client::Client;
    use clock::FakeSystemClock;
    use http_client::FakeHttpClient;
    use schemars::JsonSchema;
    use serde::Serialize;
    use settings::Settings;

    #[test]
    fn test_json_schema() {
        #[derive(Serialize, JsonSchema)]
        struct GetWeatherTool {
            location: String,
        }

        let schema = schema::json_schema_for::<GetWeatherTool>(
            language_model::LanguageModelToolSchemaFormat::JsonSchema,
        )
        .unwrap();

        assert_eq!(
            schema,
            serde_json::json!({
                "type": "object",
                "properties": {
                    "location": {
                        "type": "string"
                    }
                },
                "required": ["location"],
                "additionalProperties": false
            })
        );
    }

    #[gpui::test]
    fn test_builtin_tool_schema_compatibility(cx: &mut App) {
        settings::init(cx);
        AgentSettings::register(cx);

        let client = Client::new(
            Arc::new(FakeSystemClock::new()),
            FakeHttpClient::with_200_response(),
            cx,
        );
        language_model::init(client.clone(), cx);
        crate::init(client.http_client(), cx);

        for tool in ToolRegistry::global(cx).tools() {
            let actual_schema = tool
                .input_schema(language_model::LanguageModelToolSchemaFormat::JsonSchemaSubset)
                .unwrap();
            let mut expected_schema = actual_schema.clone();
            assistant_tool::adapt_schema_to_format(
                &mut expected_schema,
                language_model::LanguageModelToolSchemaFormat::JsonSchemaSubset,
            )
            .unwrap();

            let error_message = format!(
                "Tool schema for `{}` is not compatible with `language_model::LanguageModelToolSchemaFormat::JsonSchemaSubset` (Gemini Models).\n\
                Are you using `schema::json_schema_for<T>(format)` to generate the schema?",
                tool.name(),
            );

            assert_eq!(actual_schema, expected_schema, "{}", error_message)
        }
    }

    #[gpui::test]
    fn memory_tool_registered(cx: &mut App) {
        assistant_tool::init(cx);
        let registry = ToolRegistry::global(cx);
        registry.register_tool(MemoryTool);

        let names: Vec<String> = registry.tools().iter().map(|t| t.name()).collect();

        assert!(
            names.contains(&"memory".to_string()),
            "MemoryTool not registered"
        );
    }

    #[gpui::test]
    fn chat_history_tool_registered(cx: &mut App) {
        assistant_tool::init(cx);
        let registry = ToolRegistry::global(cx);
        registry.register_tool(ChatHistoryTool);

        let names: Vec<String> = registry.tools().iter().map(|t| t.name()).collect();
        assert!(
            names.contains(&"chat_history".to_string()),
            "ChatHistoryTool not registered"
        );
    }

    #[gpui::test]
    fn chat_history_tool_schema_has_operation_object(cx: &mut App) {
        assistant_tool::init(cx);
        let tool = ChatHistoryTool;
        let schema = tool
            .input_schema(language_model::LanguageModelToolSchemaFormat::JsonSchemaSubset)
            .expect("schema generation");

        assert!(
            schema.get("properties").is_some(),
            "schema missing top-level properties object"
        );
        let props = schema
            .get("properties")
            .and_then(|p| p.get("operation"))
            .expect("schema.properties.operation missing");
        assert_eq!(
            props.get("type").and_then(|t| t.as_str()),
            Some("object"),
            "operation must be an object"
        );
        let op_required = props.get("required");
        assert!(
            op_required.is_none() || op_required.unwrap().is_array(),
            "operation.required should be an array if present"
        );
    }
}
