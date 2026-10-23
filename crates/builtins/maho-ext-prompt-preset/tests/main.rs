//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "deepseek_v4.rs"]
mod deepseek_v4;
#[path = "dispatch.rs"]
mod dispatch;
#[path = "execution_tooling.rs"]
mod execution_tooling;
#[path = "faux_session.rs"]
mod faux_session;
#[path = "file_operations.rs"]
mod file_operations;
#[path = "hooks.rs"]
mod hooks;
#[path = "main_hooks.rs"]
mod main_hooks;
#[path = "routing.rs"]
mod routing;
