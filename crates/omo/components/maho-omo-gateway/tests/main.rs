//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "prompt_tests.rs"]
mod prompt_tests;
#[path = "rules_block_tests.rs"]
mod rules_block_tests;
#[path = "store_extension_tests.rs"]
mod store_extension_tests;
