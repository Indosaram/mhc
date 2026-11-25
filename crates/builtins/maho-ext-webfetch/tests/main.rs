//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "app_shell.rs"]
mod app_shell;
#[path = "dom_source_cases.rs"]
mod dom_source_cases;
#[path = "golden.rs"]
mod golden;
#[path = "native_render.rs"]
mod native_render;
#[path = "reader_mode.rs"]
mod reader_mode;
#[path = "signed_in_hosts.rs"]
mod signed_in_hosts;
