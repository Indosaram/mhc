use maho_omo_computer_use::settings::resolve_omo_computer_settings;
use serde_json::json;

#[test]
fn test_to_computer_settings_input_none() {
    let resolved = resolve_omo_computer_settings(None, "darwin").expect("resolved");
    assert!(resolved.enabled);
}

#[test]
fn test_to_computer_settings_input_populated() {
    let settings = json!({
        "enabled": true,
        "display": "1",
        "max_width": 1920,
        "max_height": 1080,
        "screenshot_max_bytes": 2000000,
        "stop_hotkey": "ctrl+alt+s",
        "allow_host_relay_only_stop": true,
        "macos_canary": "session",
        "engine_path": "/bin/custom-engine",
        "cua_adapter": true
    });
    let resolved = resolve_omo_computer_settings(Some(&settings), "darwin").expect("resolved");
    assert!(resolved.enabled);
    assert_eq!(&resolved.display, "1");
    assert_eq!(resolved.max_width, 1920);
    assert_eq!(resolved.engine_path.as_deref(), Some("/bin/custom-engine"));
    assert!(resolved.cua_adapter);
}

#[test]
fn test_resolve_omo_computer_settings_supported() {
    let settings = json!({
        "enabled": true
    });
    let resolved = resolve_omo_computer_settings(Some(&settings), "darwin").expect("resolved");
    assert!(resolved.enabled);
}

#[test]
fn test_resolve_omo_computer_settings_unsupported_host_disabled() {
    let resolved = resolve_omo_computer_settings(None, "freebsd").expect("resolved");
    assert!(!resolved.enabled);
}

#[test]
fn test_resolve_omo_computer_settings_default_stop_hotkey() {
    let settings = json!({
        "enabled": true
    });
    let resolved = resolve_omo_computer_settings(Some(&settings), "darwin").expect("resolved");
    assert!(!resolved.stop_hotkey.is_empty());
}
