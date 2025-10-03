// NOTE: This crate can expose dynamic adapters for registering agent2 thread
// context-management tools (e.g. list_history, memory) via the `agent_tool_adapter`
// module. The adapter is always compiled now that the former feature gate was removed.
pub mod client;
pub mod listener;
pub mod protocol;
#[cfg(any(test, feature = "test-support"))]
pub mod test;
pub mod transport;
pub mod types;

use std::path::Path;
use std::sync::Arc;
use std::{fmt::Display, path::PathBuf};

use anyhow::Result;
use client::Client;
use gpui::AsyncApp;
use parking_lot::RwLock;
pub use settings::ContextServerCommand;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ContextServerId(pub Arc<str>);

impl Display for ContextServerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

enum ContextServerTransport {
    Stdio(ContextServerCommand, Option<PathBuf>),
    Custom(Arc<dyn crate::transport::Transport>),
}

pub struct ContextServer {
    id: ContextServerId,
    client: RwLock<Option<Arc<crate::protocol::InitializedContextServerProtocol>>>,
    configuration: ContextServerTransport,
}

impl ContextServer {
    pub fn stdio(
        id: ContextServerId,
        command: ContextServerCommand,
        working_directory: Option<Arc<Path>>,
    ) -> Self {
        Self {
            id,
            client: RwLock::new(None),
            configuration: ContextServerTransport::Stdio(
                command,
                working_directory.map(|directory| directory.to_path_buf()),
            ),
        }
    }

    pub fn new(id: ContextServerId, transport: Arc<dyn crate::transport::Transport>) -> Self {
        Self {
            id,
            client: RwLock::new(None),
            configuration: ContextServerTransport::Custom(transport),
        }
    }

    pub fn id(&self) -> ContextServerId {
        self.id.clone()
    }

    pub fn client(&self) -> Option<Arc<crate::protocol::InitializedContextServerProtocol>> {
        self.client.read().clone()
    }

    pub async fn start(&self, cx: &AsyncApp) -> Result<()> {
        self.initialize(self.new_client(cx)?).await
    }

    /// Starts the context server, making sure handlers are registered before initialization happens
    pub async fn start_with_handlers(
        &self,
        notification_handlers: Vec<(
            &'static str,
            Box<dyn 'static + Send + FnMut(serde_json::Value, AsyncApp)>,
        )>,
        cx: &AsyncApp,
    ) -> Result<()> {
        let client = self.new_client(cx)?;
        for (method, handler) in notification_handlers {
            client.on_notification(method, handler);
        }
        self.initialize(client).await
    }

    fn new_client(&self, cx: &AsyncApp) -> Result<Client> {
        Ok(match &self.configuration {
            ContextServerTransport::Stdio(command, working_directory) => Client::stdio(
                client::ContextServerId(self.id.0.clone()),
                client::ModelContextServerBinary {
                    executable: Path::new(&command.path).to_path_buf(),
                    args: command.args.clone(),
                    env: command.env.clone(),
                    timeout: command.timeout,
                },
                working_directory,
                cx.clone(),
            )?,
            ContextServerTransport::Custom(transport) => Client::new(
                client::ContextServerId(self.id.0.clone()),
                self.id().0,
                transport.clone(),
                None,
                cx.clone(),
            )?,
        })
    }

    async fn initialize(&self, client: Client) -> Result<()> {
        log::debug!("starting context server {}", self.id);
        let protocol = crate::protocol::ModelContextProtocol::new(client);
        let client_info = types::Implementation {
            name: "Zed".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        };
        let initialized_protocol = protocol.initialize(client_info).await?;

        log::debug!(
            "context server {} initialized: {:?}",
            self.id,
            initialized_protocol.initialize,
        );

        *self.client.write() = Some(Arc::new(initialized_protocol));
        Ok(())
    }

    pub fn stop(&self) -> Result<()> {
        let mut client = self.client.write();
        if let Some(protocol) = client.take() {
            drop(protocol);
        }
        Ok(())
    }
}

/// Adapter for exposing agent2 (Thread) tools through the context server's MCP interface.
///
/// This is a lightweight scaffold intended to be expanded when the agent thread
/// execution routing (foreground task invocation + result marshaling) is wired in.
/// For now it provides:
/// - A descriptor type (`AgentToolExport`)
/// - A helper to register exported tools onto an `McpServer` using its
///   dynamic registration API (`add_dynamic_tool`)
/// - A placeholder handler that returns an error until real invocation logic
///   (calling back into the agent thread) is implemented.
///
/// The real implementation will:
/// 1. Accept an `Arc<agent2::Thread>` plus a selected subset of tools
/// 2. For each tool, extract its JSON schema (already produced in agent2)
/// 3. Create a handler closure that schedules execution on the foreground thread
/// 4. Stream incremental output (if desired) via `ToolResponseContent` events
pub mod agent_tool_adapter {
    use anyhow::{Result, anyhow};
    use gpui::AsyncApp;
    use serde_json::Value;
    use std::sync::Arc;

    use crate::listener::McpServer;
    use crate::types::{ToolAnnotations, ToolResponseContent};

    /// Exported agent tool descriptor (schema + metadata).
    pub struct AgentToolExport {
        pub name: &'static str,
        pub description: Option<String>,
        pub input_schema: Value,
        pub output_schema: Option<Value>,
        pub read_only: bool,
    }

    impl AgentToolExport {
        pub fn new(
            name: &'static str,
            description: Option<String>,
            input_schema: Value,
            output_schema: Option<Value>,
            read_only: bool,
        ) -> Self {
            Self {
                name,
                description,
                input_schema,
                output_schema,
                read_only,
            }
        }
    }

    /// Registers a collection of agent tool exports on the supplied MCP server.
    ///
    /// Currently installs placeholder handlers that return an informative error.
    /// Once the agent thread invocation bridge is available, replace the handler
    /// body to:
    ///   - Serialize arguments
    ///   - Dispatch onto the foreground thread (e.g. via a channel or spawn helper)
    ///   - Await tool completion
    ///   - Map the structured result into `ToolResponseContent`
    pub fn register_agent_tools(
        server: &mut McpServer,
        exports: &[AgentToolExport],
        _agent_thread: Option<Arc<()>>, // Placeholder for future: Arc<agent2::Thread>
    ) {
        for export in exports {
            // Build annotations
            let annotations = ToolAnnotations {
                title: Some(export.name.to_string()),
                read_only_hint: Some(export.read_only),
                destructive_hint: if export.read_only { None } else { Some(true) },
                idempotent_hint: None,
                open_world_hint: None,
            };

            // Placeholder handler: returns an explanatory error until bridged.
            // NOTE: Relies on `add_dynamic_tool` accepting a closure with the expected signature.
            server.add_dynamic_tool(
                export.name,
                export.description.clone(),
                export.input_schema.clone(),
                export.output_schema.clone(),
                Some(annotations),
                Box::new(|_args: Option<Value>, cx: &mut AsyncApp| {
                    cx.spawn(async move |_| {
                        Err(anyhow!(
                            "agent tool '{}' not yet wired for execution (adapter placeholder)",
                            export.name
                        ))
                        .map(|_| super::listener::ToolResponse {
                            content: vec![ToolResponseContent::Text {
                                text: "unreachable".into(),
                            }],
                            structured_content: serde_json::Value::Null,
                        })
                    })
                }),
            );
        }
    }

    /// Convenience helper to register just the two core context compaction tools if present.
    pub fn register_core_context_tools(
        server: &mut McpServer,
        list_history_schema: Value,
        memory_schema: Value,
    ) {
        let tools = [
            AgentToolExport::new(
                "list_history",
                Some("Enumerate conversation history indices with previews to plan compression.".into()),
                list_history_schema,
                None,
                true,
            ),
            AgentToolExport::new(
                "memory",
                Some("Archive, restore, and list compacted conversation segments to manage context window usage.".into()),
                memory_schema,
                None,
                false,
            ),
        ];
        register_agent_tools(server, &tools, None);
    }
}
