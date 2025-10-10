mod agent;
mod db;
mod history_store;
mod native_agent_server;
mod templates;
mod thread;
mod token_usage;
mod tool_schema;
mod tools;
mod embedded_mcp_server;

#[cfg(test)]
mod tests;

pub use agent::*;
pub use db::*;
pub use history_store::*;
pub use native_agent_server::NativeAgentServer;
pub use templates::*;
pub use thread::*;
pub use token_usage::*;
pub use tools::*;
pub fn init_agent2(cx: &mut gpui::App) {
    embedded_mcp_server::init(cx);
}
