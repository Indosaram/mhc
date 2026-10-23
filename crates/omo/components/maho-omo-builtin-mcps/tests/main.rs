//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "declaration_consumer.rs"]
pub mod declaration_consumer;
#[path = "declarations.rs"]
pub mod declarations;
#[path = "host_declaration_contract.rs"]
pub mod host_declaration_contract;
#[path = "registration.rs"]
pub mod registration;

// Helpers several test modules share, declared once so each file compiles once.
#[path = "support/mod.rs"]
pub mod support;
