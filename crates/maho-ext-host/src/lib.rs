//! Extension host: loads the statically registered extensions, runs their hooks and tools against a
//! session (`runner`, `loader`, `wrapper`), and provides the UI, exec and OAuth contexts they use.

pub mod runner;
pub use runner::*;
pub mod exec;
pub mod extension_handle_ledger;
pub mod kernel_tools_context;
pub mod loader;
pub mod mcp_server_registry;
pub mod notice;
pub mod oauth;
pub mod provider_login_start;
pub mod session_start_turn_gate;
pub mod tool_hook_wait;
pub mod ui;
pub mod wrapper;
