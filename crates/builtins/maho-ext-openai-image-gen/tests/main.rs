//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "externalize.rs"]
pub mod externalize;
#[path = "gate.rs"]
pub mod gate;
#[path = "inject.rs"]
pub mod inject;
#[path = "native_factory.rs"]
pub mod native_factory;
#[path = "session_arbitration.rs"]
pub mod session_arbitration;
#[path = "session_extension.rs"]
pub mod session_extension;

// Helpers several test modules share, declared once so each file compiles once.
#[path = "../../maho-ext-imagegen/tests/support.rs"]
pub mod imagegen_support;
