//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "command.rs"]
pub mod command;
#[path = "native_account.rs"]
pub mod native_account;
#[path = "native_btw.rs"]
pub mod native_btw;
#[path = "native_imagegen.rs"]
pub mod native_imagegen;
#[path = "native_openai_imagegen.rs"]
pub mod native_openai_imagegen;
#[path = "native_reasoning.rs"]
pub mod native_reasoning;
#[path = "native_service_tier.rs"]
pub mod native_service_tier;

// Helpers several test modules share, declared once so each file compiles once.
#[path = "native_account/support.rs"]
pub mod support;
#[path = "native_account/native_state.rs"]
pub mod native_state;
