use std::collections::HashSet;
use std::sync::{Mutex, LazyLock};
static WARNINGS: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
pub fn warn_deprecation(message: &str) { let mut warnings = WARNINGS.lock().unwrap_or_else(|p| p.into_inner()); if warnings.insert(message.to_owned()) { eprintln!("Deprecation warning: {message}"); } }
pub fn clear_deprecation_warnings_for_tests() { WARNINGS.lock().unwrap_or_else(|p| p.into_inner()).clear(); }
