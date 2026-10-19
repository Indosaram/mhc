use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default)]
pub struct DesktopEngineLocatorOptions {
    pub platform: Option<String>,
    pub arch: Option<String>,
    pub libc: Option<String>,
    pub runtime_dir: Option<String>,
    pub exec_dir: Option<String>,
    pub package_dir: Option<String>,
    pub repo_root: Option<String>,
    pub is_quarantined: Option<fn(&str) -> bool>,
}

impl PartialEq for DesktopEngineLocatorOptions {
    fn eq(&self, other: &Self) -> bool {
        self.platform == other.platform
            && self.arch == other.arch
            && self.libc == other.libc
            && self.runtime_dir == other.runtime_dir
            && self.exec_dir == other.exec_dir
            && self.package_dir == other.package_dir
            && self.repo_root == other.repo_root
            && matches!((self.is_quarantined, other.is_quarantined), (None, None) | (Some(_), Some(_)))
    }
}

impl Eq for DesktopEngineLocatorOptions {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesktopEngineLocateDiagnostic {
    pub host: String,
    pub attempted_paths: Vec<String>,
    pub code: String,
    pub message: String,
    pub cause: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstalledEngine {
    Installed { path: String, source: String },
    Diagnostic(DesktopEngineLocateDiagnostic),
    NotInstalled(DesktopEngineLocateDiagnostic),
}

pub fn get_desktop_engine_host(platform: &str, arch: Option<&str>, libc: Option<&str>) -> String {
    let arch_str = arch.unwrap_or("x64");
    if platform == "linux" && libc == Some("musl") {
        format!("{platform}-{arch_str}-musl")
    } else {
        format!("{platform}-{arch_str}")
    }
}

pub fn is_quarantined_file(_path: &str, _platform: &str) -> bool {
    false
}

pub fn desktop_engine_release_asset_name(host: &str) -> Option<String> {
    match host {
        "darwin-x64" | "darwin-arm64" => Some(format!("senpi-desktop-engine-{host}")),
        "linux-x64" => Some(format!("senpi-desktop-engine-{host}")),
        "win32-x64" => Some(format!("senpi-desktop-engine-{host}.exe")),
        _ => None,
    }
}

pub fn explicit_path_diagnostic(
    engine_path: &str,
    host: &str,
    platform: &str,
    is_quarantined: Option<fn(&str) -> bool>,
) -> Option<DesktopEngineLocateDiagnostic> {
    let p = Path::new(engine_path);
    let base = DesktopEngineLocateDiagnostic {
        host: host.to_string(),
        attempted_paths: vec![engine_path.to_string()],
        code: "native-unavailable".to_string(),
        message: format!("No senpi-desktop-engine binary is available for {host}."),
        cause: String::new(),
    };

    if !p.exists() {
        return Some(DesktopEngineLocateDiagnostic {
            cause: format!("{engine_path}: missing"),
            ..base
        });
    }

    let is_q = is_quarantined.unwrap_or(|path| is_quarantined_file(path, ""));
    if is_q(engine_path) {
        return Some(DesktopEngineLocateDiagnostic {
            code: "quarantined".to_string(),
            message: format!("The senpi-desktop-engine binary for {host} is quarantined by macOS Gatekeeper: {engine_path}."),
            cause: format!("{engine_path}: blocked because com.apple.quarantine is present (macOS Gatekeeper)"),
            ..base
        });
    }

    if platform == "win32" {
        if engine_path.to_lowercase().ends_with(".exe") {
            None
        } else {
            Some(DesktopEngineLocateDiagnostic {
                cause: format!("{engine_path}: not executable (expected a .exe file)"),
                ..base
            })
        }
    } else {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = p.metadata()
                && (meta.permissions().mode() & 0o111) == 0 {
                return Some(DesktopEngineLocateDiagnostic {
                    cause: format!("{engine_path}: not executable (chmod +x)"),
                    ..base
                });
            }
        }
        None
    }
}

pub fn get_desktop_engine_candidate_paths(
    options: &DesktopEngineLocatorOptions,
    platform: &str,
    host: &str,
) -> Vec<String> {
    let file_name = if platform == "win32" {
        "senpi-desktop-engine.exe"
    } else {
        "senpi-desktop-engine"
    };

    let mut paths = Vec::new();
    if let Some(ref rd) = options.runtime_dir {
        paths.push(Path::new(rd).join(file_name).to_string_lossy().to_string());
    }
    if let Some(ref ed) = options.exec_dir {
        paths.push(Path::new(ed).join(file_name).to_string_lossy().to_string());
    }
    if let Some(ref pd) = options.package_dir {
        paths.push(Path::new(pd).join("prebuilds").join(host).join(file_name).to_string_lossy().to_string());
    }
    if let Some(ref rr) = options.repo_root {
        paths.push(Path::new(rr).join("dist").join(file_name).to_string_lossy().to_string());
    }
    paths
}

pub fn resolve_installed_engine(
    engine_path: Option<&str>,
    env: &std::collections::BTreeMap<String, String>,
    version: Option<&str>,
    locator_options: &DesktopEngineLocatorOptions,
) -> InstalledEngine {
    let platform = locator_options
        .platform
        .clone()
        .unwrap_or_else(|| std::env::consts::OS.to_string());
    let host = get_desktop_engine_host(
        &platform,
        locator_options.arch.as_deref(),
        locator_options.libc.as_deref(),
    );

    if let Some(ep) = engine_path {
        if let Some(diag) = explicit_path_diagnostic(ep, &host, &platform, locator_options.is_quarantined) {
            return InstalledEngine::Diagnostic(diag);
        } else {
            return InstalledEngine::Installed {
                path: ep.to_string(),
                source: "explicit".to_string(),
            };
        }
    }

    let candidates = get_desktop_engine_candidate_paths(locator_options, &platform, &host);
    for cand in &candidates {
        let p = Path::new(cand);
        if p.exists() {
            let source = if locator_options.runtime_dir.as_ref().is_some_and(|rd| cand.starts_with(rd)) {
                "runtime-dir"
            } else if locator_options.exec_dir.as_ref().is_some_and(|ed| cand.starts_with(ed)) {
                "sidecar"
            } else if locator_options.package_dir.as_ref().is_some_and(|pd| cand.starts_with(pd)) {
                "package-prebuild"
            } else {
                "dev-build"
            };
            return InstalledEngine::Installed {
                path: cand.clone(),
                source: source.to_string(),
            };
        }
    }

    if let Some(ver) = version {
        let home = env
            .get("HOME")
            .or_else(|| env.get("USERPROFILE"))
            .cloned()
            .unwrap_or_else(|| "/tmp".to_string());
        let cache_path = PathBuf::from(home)
            .join(".omo")
            .join("cache")
            .join("senpi-desktop-engine")
            .join(&host)
            .join(ver)
            .join(if platform == "win32" { "senpi-desktop-engine.exe" } else { "senpi-desktop-engine" });
        if cache_path.exists() {
            return InstalledEngine::Installed {
                path: cache_path.to_string_lossy().to_string(),
                source: "cache".to_string(),
            };
        }
    }

    let base = DesktopEngineLocateDiagnostic {
        host: host.clone(),
        attempted_paths: candidates,
        code: "native-unavailable".to_string(),
        message: format!("No senpi-desktop-engine binary is available for {host}."),
        cause: format!("No engine found for {host}"),
    };

    if desktop_engine_release_asset_name(&host).is_none() {
        InstalledEngine::Diagnostic(base)
    } else {
        InstalledEngine::NotInstalled(base)
    }
}
