use crate::tool::types::EvalRuntimeInfo;

pub fn js_runtime_info(node_version: &str, bun_version: Option<&str>, exec_path: &str) -> EvalRuntimeInfo {
    match bun_version.filter(|version| !version.is_empty()) {
        Some(version) => EvalRuntimeInfo { name: "bun".into(), version: version.into(), path: Some(exec_path.into()) },
        None => EvalRuntimeInfo { name: "node".into(), version: node_version.into(), path: Some(exec_path.into()) },
    }
}

pub fn js_runtime_label(node_version: &str, bun_version: Option<&str>) -> String {
    let runtime = js_runtime_info(node_version, bun_version, "");
    format!("{} {}", runtime.name, runtime.version)
}

/// Identity of the bun/node binary the native JS kernel spawns, probed with `--version` on PATH.
/// Native counterpart of `jsRuntimeInfo()` reading `process.versions` in the host process.
pub fn probe_js_runtime_info(environment: &std::collections::HashMap<String, String>) -> EvalRuntimeInfo {
    let cwd = std::env::current_dir().unwrap_or_default();
    let probe = |command: &str| -> Option<(String, String)> {
        let path = crate::interpreters::resolve_command::resolve_command_path(command, environment, &cwd, cfg!(windows))?;
        let output = std::process::Command::new(&path).arg("--version").output().ok().filter(|output| output.status.success())?;
        let version = String::from_utf8_lossy(&output.stdout).trim().trim_start_matches('v').to_owned();
        (!version.is_empty()).then(|| (version, path.to_string_lossy().into_owned()))
    };
    match probe("bun") {
        Some((version, path)) => js_runtime_info("", Some(&version), &path),
        None => {
            let (version, path) = probe("node").unwrap_or_else(|| (String::new(), "node".into()));
            js_runtime_info(&version, None, &path)
        }
    }
}
