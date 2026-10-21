use serde::{Deserialize, Serialize};

pub const JS_ISOLATION_ENVIRONMENT_FLAG: &str = "SENPI_CODEMODE_JS_ISOLATION";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JsIsolation {
    #[default]
    Worker,
    Process,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IsolationSettings {
    pub js: JsIsolation,
}

pub fn resolve_js_isolation(
    settings: &super::settings::CodemodeSettings,
    env: &super::settings::Environment,
) -> JsIsolation {
    match env.get(JS_ISOLATION_ENVIRONMENT_FLAG).map(String::as_str) {
        Some("worker") => JsIsolation::Worker,
        Some("process") => JsIsolation::Process,
        _ => settings.isolation.js,
    }
}
