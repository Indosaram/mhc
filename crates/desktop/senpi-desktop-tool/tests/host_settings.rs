//! Tests matching upstream `test/host-settings.test.ts`.

use senpi_desktop_tool::host_policy::is_supported_host;
use senpi_desktop_tool::settings::{
    resolve_computer_settings, AuditLogSettings, ComputerSettings,
    ScreenshotGcSettings,
};
use serde_json::json;

#[test]
fn is_supported_host_supports_darwin() {
    assert!(is_supported_host("darwin"));
}

#[test]
fn is_supported_host_supports_linux() {
    assert!(is_supported_host("linux"));
}

#[test]
fn is_supported_host_supports_win32() {
    assert!(is_supported_host("win32"));
}

#[test]
fn is_supported_host_does_not_support_freebsd() {
    assert!(!is_supported_host("freebsd"));
}

#[test]
fn resolve_computer_settings_fills_every_default_when_absent() {
    let settings = resolve_computer_settings(None, "darwin").expect("resolve");
    assert_eq!(
        settings,
        ComputerSettings {
            enabled: true,
            display: "all".to_string(),
            max_width: 3840,
            max_height: 2400,
            screenshot_max_bytes: 5_000_000,
            stop_hotkey: "ctrl+alt+cmd+escape".to_string(),
            allow_host_relay_only_stop: false,
            macos_canary: "session".to_string(),
            audit_log: AuditLogSettings { enabled: true },
            screenshot_gc: ScreenshotGcSettings {
                enabled: true,
                stale_ms: 43_200_000,
                scan_interval_ms: 1_800_000,
            },
            engine_path: None,
            cua_adapter: false,
        }
    );
}

#[test]
fn resolve_computer_settings_defaults_enabled_off_on_unsupported_host() {
    let raw = json!({});
    let settings = resolve_computer_settings(Some(&raw), "freebsd").expect("resolve");
    assert!(!settings.enabled);
    assert_eq!(settings.stop_hotkey, "ctrl+alt+shift+escape");
}

#[test]
fn resolve_computer_settings_keeps_explicit_values_over_host_defaults() {
    let raw = json!({
        "enabled": false,
        "stopHotkey": "ctrl+alt+shift+f12",
        "screenshotGc": { "staleMs": 60_000 }
    });
    let settings = resolve_computer_settings(Some(&raw), "darwin").expect("resolve");
    assert!(!settings.enabled);
    assert_eq!(settings.stop_hotkey, "ctrl+alt+shift+f12");
    assert_eq!(settings.screenshot_gc.stale_ms, 60_000);
}

#[test]
fn resolve_computer_settings_rejects_unknown_key_and_wrongly_typed_value() {
    let raw = json!({
        "maxWidth": "wide",
        "stopChord": "ctrl+escape"
    });
    let err = resolve_computer_settings(Some(&raw), "linux").unwrap_err();
    assert!(err.issues.iter().any(|i| i.contains("unknown property 'stopChord'")));
    assert!(err.issues.iter().any(|i| i.contains("maxWidth")));
}
