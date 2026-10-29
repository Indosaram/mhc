//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "auth_tests.rs"]
mod auth_tests;
#[path = "client_tests.rs"]
mod client_tests;
#[path = "index_tests.rs"]
mod index_tests;
#[path = "params_tests.rs"]
mod params_tests;
#[path = "skill_tests.rs"]
mod skill_tests;
#[path = "tool_tests.rs"]
mod tool_tests;
