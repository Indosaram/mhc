//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "dispatch.rs"]
mod dispatch;
#[path = "faux_session.rs"]
mod faux_session;
#[path = "input.rs"]
mod input;
