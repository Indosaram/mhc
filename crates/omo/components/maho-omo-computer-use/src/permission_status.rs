use std::collections::BTreeMap;

use crate::engine_probe::{COMPUTER_USE_DOCTOR_TIMEOUT_MS, probe_computer_use_engine};
use crate::engine_source::omo_release_version;
use crate::installed_engine::{
    DesktopEngineLocatorOptions, InstalledEngine, resolve_installed_engine,
};

pub async fn describe_engine_permissions(
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

    let path = match resolved {
        InstalledEngine::Installed { path, .. } => path,
        _ => {
            return "OS permissions: unknown until first use (no usable installed engine)"
                .to_string();
        }
    };

    let probed = probe_computer_use_engine(&path, env, COMPUTER_USE_DOCTOR_TIMEOUT_MS).await;
    match probed {
        crate::engine_probe::EngineProbeResult::Ok(p) => {
            let caps = &p.capabilities;
            format!(
                "OS permissions: capturePermission={} inputPermission={} axPermission={} (passive probe)",
                caps.capture_permission, caps.input_permission, caps.ax_permission
            )
        }
        crate::engine_probe::EngineProbeResult::LaunchContextUnavailable { message, .. } => {
            format!("OS permissions: unknown (launch-context-unavailable: {message})")
        }
        crate::engine_probe::EngineProbeResult::Err { code, message } => {
            format!("OS permissions: unknown ({code}: {message})")
        }
    }
}
