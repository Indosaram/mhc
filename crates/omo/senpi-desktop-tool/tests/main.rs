//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "command.rs"]
pub mod command;
#[path = "cua_adapter.rs"]
pub mod cua_adapter;
#[path = "execute.rs"]
pub mod execute;
#[path = "host_settings.rs"]
pub mod host_settings;
#[path = "permission.rs"]
pub mod permission;
#[path = "permission_guidance.rs"]
pub mod permission_guidance;
#[path = "tool_definition.rs"]
pub mod tool_definition;

// Helpers several test modules share, declared once so each file compiles once.
#[path = "fixtures.rs"]
pub mod fixtures;
