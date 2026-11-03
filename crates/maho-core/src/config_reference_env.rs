//! Port of senpi packages/coding-agent/src/core/config-reference-env.ts.
//!
//! One-time notices for configuration environment values: a value that looks like a `{file:` / `{env:`
//! reference is inserted literally, and an unset template variable is reported once per process.

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

static WARNED_VARIABLES: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

fn first_warning(name: &str) -> bool {
    WARNED_VARIABLES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(name.to_owned())
}

fn looks_like_reference(value: &str) -> bool {
    value.contains("{file:") || value.contains("{env:")
}

/// The notice text, or `None` once the variable was already reported (or the value is plain).
pub fn opaque_config_environment_notice(name: &str, value: Option<&str>) -> Option<String> {
    let value = value?;
    if !looks_like_reference(value) || !first_warning(name) {
        return None;
    }
    Some(format!(
        "{name}: environment value contains reference-looking text; inserted literally"
    ))
}

pub fn warn_opaque_config_environment(name: &str, value: Option<&str>) {
    if let Some(notice) = opaque_config_environment_notice(name, value) {
        crate::output_guard::maho_write_stderr(&format!("{notice}\n"));
    }
}

pub fn missing_config_environment_notice(name: &str) -> Option<String> {
    first_warning(name).then(|| format!("{name}: configuration environment variable is unset"))
}

pub fn warn_missing_config_environment(name: &str) {
    if let Some(notice) = missing_config_environment_notice(name) {
        crate::output_guard::maho_write_stderr(&format!("{notice}\n"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// senpi test/suite/regressions/3858-reference-values.test.ts: each variable warns once per process.
    #[test]
    fn notices_are_printed_once_per_variable() {
        let name = "MAHO_CONFIG_REFERENCE_ENV_TEST_OPAQUE";
        assert_eq!(
            opaque_config_environment_notice(name, Some("{file:/x}")).as_deref(),
            Some(
                "MAHO_CONFIG_REFERENCE_ENV_TEST_OPAQUE: environment value contains reference-looking text; inserted literally"
            )
        );
        assert_eq!(
            opaque_config_environment_notice(name, Some("{file:/x}")),
            None
        );
        assert_eq!(
            opaque_config_environment_notice("MAHO_CONFIG_REFERENCE_ENV_TEST_PLAIN", Some("plain")),
            None
        );
        let missing = "MAHO_CONFIG_REFERENCE_ENV_TEST_MISSING";
        assert!(missing_config_environment_notice(missing).is_some());
        assert_eq!(missing_config_environment_notice(missing), None);
    }
}
