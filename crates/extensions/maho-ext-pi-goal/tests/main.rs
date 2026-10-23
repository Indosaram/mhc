//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "command.rs"]
mod command;
#[path = "continuation.rs"]
mod continuation;
#[path = "format.rs"]
mod format;
#[path = "index.rs"]
mod index;
#[path = "lifecycle.rs"]
mod lifecycle;
#[path = "store.rs"]
mod store;
#[path = "store_errors.rs"]
mod store_errors;
#[path = "tool_registration.rs"]
mod tool_registration;
#[path = "transitions.rs"]
mod transitions;
#[path = "turn_usage.rs"]
mod turn_usage;
#[path = "validation.rs"]
mod validation;
