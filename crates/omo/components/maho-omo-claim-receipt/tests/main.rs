//! Single integration-test binary for this crate: each file under tests/ is one module, so the crate
//! links one test executable instead of one per file.

pub mod support;

pub mod classify;
pub mod evidence;
pub mod index;
pub mod invocations;
pub mod r#match;
