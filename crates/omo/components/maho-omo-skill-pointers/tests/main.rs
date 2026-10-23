//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "skill_pointers.rs"]
pub mod skill_pointers;
#[path = "skill_pointers_companion.rs"]
pub mod skill_pointers_companion;
#[path = "skill_pointers_queue_scope.rs"]
pub mod skill_pointers_queue_scope;
#[path = "skill_pointers_suppression.rs"]
pub mod skill_pointers_suppression;

// Helpers several test modules share, declared once so each file compiles once.
#[path = "support/mod.rs"]
pub mod support;
