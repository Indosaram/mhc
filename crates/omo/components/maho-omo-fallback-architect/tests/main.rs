//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "architect_gate.rs"]
mod architect_gate;
#[path = "directive.rs"]
mod directive;
#[path = "faux_session.rs"]
mod faux_session;
#[path = "lifecycle.rs"]
mod lifecycle;
#[path = "notice.rs"]
mod notice;
#[path = "tip_message.rs"]
mod tip_message;
