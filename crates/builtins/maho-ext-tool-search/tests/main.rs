//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "locale_order.rs"]
mod locale_order;
#[path = "native.rs"]
mod native;
