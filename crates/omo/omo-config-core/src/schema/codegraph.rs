use serde_json::Value;

use crate::internal::validate::{
    Field, Node, array, boolean, defaulted, number, optional, required, strict_object, string,
};
use crate::schema::harness::HARNESS_IDS;

pub const CODEX_AND_OPENCODE: [&str; 2] = ["codex", "opencode"];
pub const CODEX_ONLY: [&str; 1] = ["codex"];
pub const OPENCODE_AND_OMO: [&str; 2] = ["opencode", "omo"];

pub const SETTING_HARNESS_SUPPORT: [(&str, &[&str]); 8] = [
    ("codegraph.auto_provision", &HARNESS_IDS),
    ("codegraph.daemon", &CODEX_AND_OPENCODE),
    ("codegraph.enabled", &HARNESS_IDS),
    ("codegraph.excluded_roots", &CODEX_AND_OPENCODE),
    ("codegraph.install_dir", &HARNESS_IDS),
    ("codegraph.session_start_cooldown_ms", &CODEX_ONLY),
    ("codegraph.telemetry", &HARNESS_IDS),
    ("codegraph.watch_debounce_ms", &OPENCODE_AND_OMO),
];

pub fn codegraph_setting_harness_support(setting_path: &str) -> Option<&'static [&'static str]> {
    SETTING_HARNESS_SUPPORT
        .iter()
        .find(|(path, _)| *path == setting_path)
        .map(|(_, harnesses)| *harnesses)
}

fn default_true() -> Value {
    Value::Bool(true)
}

fn default_false() -> Value {
    Value::Bool(false)
}

fn codegraph_settings_shape() -> Vec<Field> {
    vec![
        required("enabled", boolean()),
        required("auto_provision", boolean()),
        required("daemon", boolean()),
        required("telemetry", boolean()),
        optional("install_dir", string()),
        optional("watch_debounce_ms", number().with_min(0.0)),
        optional("excluded_roots", array(string())),
        optional("session_start_cooldown_ms", number().with_min(60_000.0)),
    ]
}

pub fn omo_codegraph_settings_layer_schema() -> Node {
    strict_object(
        codegraph_settings_shape()
            .into_iter()
            .map(|field| Field {
                key: field.key,
                node: field.node,
                default: None,
                required: false,
            })
            .collect(),
    )
}

pub fn omo_codegraph_settings_schema() -> Node {
    let mut fields = codegraph_settings_shape();
    fields[0] = defaulted("enabled", boolean(), default_true);
    fields[1] = defaulted("auto_provision", boolean(), default_true);
    fields[2] = defaulted("daemon", boolean(), default_true);
    fields[3] = defaulted("telemetry", boolean(), default_false);
    strict_object(fields)
}
