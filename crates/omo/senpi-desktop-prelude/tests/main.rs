//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "assets.rs"]
mod assets;
#[path = "assets_byte_identity.rs"]
mod assets_byte_identity;
#[path = "js_facade.rs"]
mod js_facade;
#[path = "method_allowlist.rs"]
mod method_allowlist;
#[path = "py_facade.rs"]
mod py_facade;
#[path = "python_command.rs"]
mod python_command;
