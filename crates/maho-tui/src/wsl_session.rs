//! Port of senpi `packages/tui/src/wsl-session.ts`.

use std::collections::HashMap;
use std::path::Path;

const WSL_ENV_MARKERS: [&str; 3] = ["WSL_DISTRO_NAME", "WSL_INTEROP", "WSLENV"];

/// Windows Subsystem for Linux session detection that survives sanitized environments (#3628).
/// `platform` uses Node's names; `proc_version_path` defaults to `/proc/version` upstream.
pub fn is_wsl_session(
    env: &HashMap<String, String>,
    platform: &str,
    proc_version_path: &Path,
) -> bool {
    if platform != "linux" {
        return false;
    }
    if WSL_ENV_MARKERS
        .iter()
        .any(|marker| env.get(*marker).is_some_and(|value| !value.is_empty()))
    {
        return true;
    }
    match std::fs::read_to_string(proc_version_path) {
        Ok(text) => {
            let lower = text.to_ascii_lowercase();
            lower.contains("microsoft") || lower.contains("wsl")
        }
        Err(_) => false,
    }
}

/// [`is_wsl_session`] for the current process environment, platform and `/proc/version`.
pub fn is_current_wsl_session() -> bool {
    let env: HashMap<String, String> = std::env::vars().collect();
    let platform = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(windows) {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        std::env::consts::OS
    };
    is_wsl_session(&env, platform, Path::new("/proc/version"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_linux_is_never_wsl() {
        let env = HashMap::from([("WSL_DISTRO_NAME".to_string(), "Ubuntu".to_string())]);
        assert!(!is_wsl_session(&env, "win32", Path::new("/nonexistent")));
    }

    #[test]
    fn env_markers_win_over_proc_version() {
        let env = HashMap::from([("WSL_INTEROP".to_string(), "/run/WSL/1".to_string())]);
        assert!(is_wsl_session(&env, "linux", Path::new("/nonexistent")));
        let empty = HashMap::from([("WSLENV".to_string(), String::new())]);
        assert!(!is_wsl_session(&empty, "linux", Path::new("/nonexistent")));
    }

    #[test]
    fn proc_version_is_matched_case_insensitively() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("version");
        std::fs::write(&path, "Linux version 5.15.0-Microsoft-standard-WSL2").unwrap();
        assert!(is_wsl_session(&HashMap::new(), "linux", &path));
        std::fs::write(&path, "Linux version 6.1.0-arch").unwrap();
        assert!(!is_wsl_session(&HashMap::new(), "linux", &path));
    }
}
