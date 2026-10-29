use crate::internal::validate::{
    Node, boolean, enumeration, non_empty_string, nonnegative_integer, optional, positive_integer,
    strict_object,
};

pub const COMPUTER_HARNESS_SUPPORT: [(&str, &[&str]); 12] = [
    ("computer.enabled", &["native"]),
    ("computer.display", &["native"]),
    ("computer.max_width", &["native"]),
    ("computer.max_height", &["native"]),
    ("computer.screenshot_max_bytes", &["native"]),
    ("computer.stop_hotkey", &["native"]),
    ("computer.allow_host_relay_only_stop", &["native"]),
    ("computer.macos_canary", &["native"]),
    ("computer.audit_log", &["native"]),
    ("computer.screenshot_gc", &["native"]),
    ("computer.engine_path", &["native"]),
    ("computer.cua_adapter", &["native"]),
];

pub fn computer_setting_harness_support(setting_path: &str) -> Option<&'static [&'static str]> {
    COMPUTER_HARNESS_SUPPORT
        .iter()
        .find(|(path, _)| *path == setting_path)
        .map(|(_, harnesses)| *harnesses)
}

fn computer_settings_fields() -> Vec<crate::internal::validate::Field> {
    vec![
        optional("enabled", boolean()),
        optional("display", non_empty_string()),
        optional("max_width", positive_integer()),
        optional("max_height", positive_integer()),
        optional("screenshot_max_bytes", positive_integer()),
        optional("stop_hotkey", non_empty_string()),
        optional("allow_host_relay_only_stop", boolean()),
        optional("macos_canary", enumeration(&["session", "off"])),
        optional(
            "audit_log",
            strict_object(vec![optional("enabled", boolean())]),
        ),
        optional(
            "screenshot_gc",
            strict_object(vec![
                optional("enabled", boolean()),
                optional("stale_ms", nonnegative_integer()),
                optional("scan_interval_ms", nonnegative_integer()),
            ]),
        ),
        optional("engine_path", non_empty_string()),
        optional("cua_adapter", boolean()),
    ]
}

/// The `computer` block: desktop computer use in omo-senpi. Every key is optional because the
/// defaults depend on the host (`enabled`, `stop_hotkey`); the computer-use component resolves them.
pub fn omo_computer_settings_layer_schema() -> Node {
    strict_object(computer_settings_fields())
}

pub fn omo_computer_settings_schema() -> Node {
    strict_object(computer_settings_fields())
}
