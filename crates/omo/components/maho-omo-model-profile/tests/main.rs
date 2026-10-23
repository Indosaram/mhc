//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "builtin_profiles.rs"]
pub mod builtin_profiles;
#[path = "chain_provider_ids.rs"]
pub mod chain_provider_ids;
#[path = "index.rs"]
pub mod index;
#[path = "request_auth.rs"]
pub mod request_auth;
#[path = "resolve.rs"]
pub mod resolve;

// Helpers several test modules share, declared once so each file compiles once.
#[path = "support/mod.rs"]
pub mod support;
