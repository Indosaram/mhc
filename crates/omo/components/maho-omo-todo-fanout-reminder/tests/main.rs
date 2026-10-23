//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "faux_session.rs"]
mod faux_session;
#[path = "faux_trigger.rs"]
mod faux_trigger;
#[path = "reminder.rs"]
mod reminder;
