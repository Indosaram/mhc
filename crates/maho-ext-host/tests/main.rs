//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "account_facade.rs"]
mod account_facade;
#[path = "account_storage.rs"]
mod account_storage;
#[path = "exec.rs"]
mod exec;
#[path = "notice.rs"]
mod notice;
#[path = "runner.rs"]
mod runner;
