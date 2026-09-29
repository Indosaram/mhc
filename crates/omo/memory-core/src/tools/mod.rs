//! Tools for interacting with git-backed memory files.

pub mod memfs;
pub mod memory;
pub mod memory_apply_patch;
pub mod patch_parser;
pub mod soul;
pub mod tool_errors;

pub use memfs::*;
pub use memory::*;
pub use memory_apply_patch::*;
pub use patch_parser::*;
pub use soul::*;
pub use tool_errors::*;

#[cfg(test)]
#[path = "memory_apply_patch_test_support.rs"]
pub mod memory_apply_patch_test_support;

#[cfg(test)]
#[path = "patch_parser_tests.rs"]
mod patch_parser_tests;

#[cfg(test)]
#[path = "memory_tests.rs"]
mod memory_tests;

#[cfg(test)]
#[path = "memory_validation_tests.rs"]
mod memory_validation_tests;

#[cfg(test)]
#[path = "memory_apply_patch_tests.rs"]
mod memory_apply_patch_tests;

#[cfg(test)]
#[path = "memory_apply_patch_validation_tests.rs"]
mod memory_apply_patch_validation_tests;

#[cfg(test)]
#[path = "memory_apply_patch_provenance_tests.rs"]
mod memory_apply_patch_provenance_tests;

#[cfg(test)]
#[path = "soul_edit_tests.rs"]
mod soul_edit_tests;
