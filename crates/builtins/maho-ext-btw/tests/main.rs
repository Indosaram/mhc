//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "cancellation.rs"]
mod cancellation;
#[path = "command.rs"]
mod command;
#[path = "context.rs"]
mod context;
#[path = "native_factory.rs"]
mod native_factory;
#[path = "session_cases.rs"]
mod session_cases;
#[path = "stream.rs"]
mod stream;
