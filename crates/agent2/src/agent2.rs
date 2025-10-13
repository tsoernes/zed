mod agent;
mod db;
pub mod embedded_mcp_server;
mod history_store;
mod native_agent_server;
mod templates;
mod thread;
mod thread_memory_backend;
mod token_usage;
mod tool_schema;
mod tools;

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
    log::info!("agent2::init_agent2 installing thread memory backend");
    crate::thread_memory_backend::install_thread_memory_backend(cx);
    log::info!("agent2::init_agent2 thread memory backend installed");
}
