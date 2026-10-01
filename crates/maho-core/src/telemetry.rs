//! Port of senpi packages/coding-agent/src/core/telemetry.ts.

use crate::brand::env_value;
use crate::config::current_env;
use crate::settings_manager::SettingsManager;

fn is_truthy_env_flag(value: Option<&str>) -> bool {
    match value {
        None => false,
        Some(value) if value.is_empty() => false,
        Some(value) => value == "1" || value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes"),
    }
}

/// Resolves install telemetry: an explicit value (or the brand TELEMETRY env var) wins, else the
/// enableInstallTelemetry setting, which defaults to true.
pub fn is_install_telemetry_enabled(settings_manager: &SettingsManager, telemetry_env: Option<&str>) -> bool {
    let value = match telemetry_env {
        Some(value) => Some(value.to_owned()),
        None => env_value("TELEMETRY", &current_env()),
    };
    match value {
        Some(value) => is_truthy_env_flag(Some(&value)),
        None => settings_manager.get_bool("enableInstallTelemetry").unwrap_or(true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truthy_flags_are_recognized() {
        assert!(is_truthy_env_flag(Some("1")));
        assert!(is_truthy_env_flag(Some("TRUE")));
        assert!(is_truthy_env_flag(Some("Yes")));
        assert!(!is_truthy_env_flag(Some("0")));
        assert!(!is_truthy_env_flag(Some("")));
        assert!(!is_truthy_env_flag(None));
    }

    fn settings() -> SettingsManager {
        SettingsManager::from_storage(
            Box::new(crate::settings_manager::InMemorySettingsStorage::default()),
            true,
        )
    }

    #[test]
    fn an_explicit_env_value_overrides_the_setting() {
        let settings = settings();
        assert!(!is_install_telemetry_enabled(&settings, Some("0")));
        assert!(is_install_telemetry_enabled(&settings, Some("true")));
    }

    #[test]
    fn the_setting_defaults_to_enabled_when_no_env_is_set() {
        let settings = settings();
        // The brand TELEMETRY env var may be set in the ambient environment; only assert the
        // setting path when it is absent.
        if env_value("TELEMETRY", &current_env()).is_none() {
            assert!(is_install_telemetry_enabled(&settings, None));
        }
    }
}
