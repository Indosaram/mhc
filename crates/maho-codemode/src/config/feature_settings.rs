use crate::kernels::js::runtime_name::runtime_name_of;
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub js: Option<JsIsolation>,
}

/// The host as the JavaScript kernel's default isolation sees it: what its own executable is, and
/// on which platform.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsIsolationHost {
    /// `"bun"` or `"node"` when the host's executable is that runtime; `None` for a compiled
    /// binary.
    pub executable: Option<&'static str>,
    pub platform: &'static str,
}

pub fn current_js_isolation_host() -> JsIsolationHost {
    let exec_path = std::env::current_exe()
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default();
    current_js_isolation_host_for(
        &exec_path,
        if cfg!(windows) {
            "win32"
        } else if cfg!(target_os = "macos") {
            "darwin"
        } else {
            std::env::consts::OS
        },
    )
}

pub fn current_js_isolation_host_for(exec_path: &str, platform: &'static str) -> JsIsolationHost {
    JsIsolationHost {
        executable: runtime_name_of(exec_path),
        platform,
    }
}

/// Without a setting, a POSIX host whose executable is `bun` runs the kernel in its own process.
/// Node.js, compiled binaries, and Windows hosts keep the worker thread by default.
pub fn default_js_isolation(host: &JsIsolationHost) -> JsIsolation {
    if host.executable == Some("bun") && host.platform != "win32" {
        JsIsolation::Process
    } else {
        JsIsolation::Worker
    }
}

/// The environment wins over the file, the file over the host default; an unknown value is
/// ignored.
pub fn resolve_js_isolation(
    settings: &super::settings::CodemodeSettings,
    env: &super::settings::Environment,
) -> JsIsolation {
    resolve_js_isolation_for_host(settings, env, &current_js_isolation_host())
}

pub fn resolve_js_isolation_for_host(
    settings: &super::settings::CodemodeSettings,
    env: &super::settings::Environment,
    host: &JsIsolationHost,
) -> JsIsolation {
    match env.get(JS_ISOLATION_ENVIRONMENT_FLAG).map(String::as_str) {
        Some("worker") => JsIsolation::Worker,
        Some("process") => JsIsolation::Process,
        _ => settings
            .isolation
            .js
            .unwrap_or_else(|| default_js_isolation(host)),
    }
}
