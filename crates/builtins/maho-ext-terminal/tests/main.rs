//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "faux_terminal.rs"]
mod faux_terminal;
#[path = "reload.rs"]
mod reload;
#[path = "terminal_lifecycle.rs"]
mod terminal_lifecycle;
