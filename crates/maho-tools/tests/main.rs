//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "contracts.rs"]
mod contracts;
#[path = "edit_parity.rs"]
mod edit_parity;
#[path = "grep_parity.rs"]
mod grep_parity;
#[path = "policy.rs"]
mod policy;
#[path = "read_blocking.rs"]
mod read_blocking;
#[path = "read_images.rs"]
mod read_images;
#[path = "shell_parity.rs"]
mod shell_parity;
#[path = "tools.rs"]
mod tools;
