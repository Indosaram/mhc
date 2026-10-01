//! Persistent goal domain modules, ported from the pinned senpi builtin.
pub mod errors;
pub mod command;
pub mod continuation;
pub mod transitions;
pub mod types;
pub mod validation;
pub mod persistence;
pub mod store;
pub mod stale_context;
pub mod store_changed_event;
pub mod last_assistant_message;
