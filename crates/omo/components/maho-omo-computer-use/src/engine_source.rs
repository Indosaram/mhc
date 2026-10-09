use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::installed_engine::{
    resolve_installed_engine, DesktopEngineLocatorOptions, InstalledEngine,
};

pub fn omo_release_version(env: &BTreeMap<String, String>) -> Option<String> {
    if let Some(pkg_dir) = env.get("OMO_PACKAGE_DIR").map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let pkg_json = Path::new(pkg_dir).join("omo").join("package.json");
        if let Ok(content) = std::fs::read_to_string(pkg_json)
            && let Ok(val) = serde_json::from_str::<serde_json::Value>(&content)
            && let Some(v) = val.get("version").and_then(|v| v.as_str()) {
            return Some(v.to_string());
        }
    }

    if let Some(bin_str) = env.get("OMO_BIN").map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let bin_path = PathBuf::from(bin_str);
        if let Some(parent) = bin_path.parent().and_then(|p| p.parent()) {
            let pkg_json = parent.join("omo-ai").join("package.json");
            if let Ok(content) = std::fs::read_to_string(pkg_json)
                && let Ok(val) = serde_json::from_str::<serde_json::Value>(&content)
                && let Some(v) = val.get("version").and_then(|v| v.as_str()) {
                return Some(v.to_string());
            }
        }
    }

    None
}

pub fn describe_engine_source(
    engine_path: Option<&str>,
    env: &BTreeMap<String, String>,
    locator_options: &DesktopEngineLocatorOptions,
) -> String {
    let resolved = resolve_installed_engine(
        engine_path,
        env,
        omo_release_version(env).as_deref(),
        locator_options,
    );
    match resolved {
        InstalledEngine::Installed { path, source } => format!("{path} ({source})"),
        InstalledEngine::Diagnostic(diag) => format!("{}: {}", diag.host, diag.message),
        InstalledEngine::NotInstalled(diag) => format!("{}: would download ({})", diag.host, diag.code),
    }
}
