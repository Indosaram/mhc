//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "faux_native_extension.rs"]
mod faux_native_extension;
#[path = "faux_native_session.rs"]
mod faux_native_session;
#[path = "faux_script.rs"]
mod faux_script;
#[path = "golden_compare.rs"]
mod golden_compare;
#[path = "vterm_golden.rs"]
mod vterm_golden;
