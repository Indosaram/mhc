//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "engine_source_tests.rs"]
mod engine_source_tests;
#[path = "engine_status_tests.rs"]
mod engine_status_tests;
#[path = "index_tests.rs"]
mod index_tests;
#[path = "permission_tests.rs"]
mod permission_tests;
#[path = "settings_tests.rs"]
mod settings_tests;
#[path = "telemetry_tests.rs"]
mod telemetry_tests;
