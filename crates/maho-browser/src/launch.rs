//! Finding a Chromium executable and the launch arguments.
//!
//! Port of oh-my-pi `packages/coding-agent/src/tools/browser/launch.ts`
//! (`systemChromiumCandidates`, `resolveSystemChromium`, `buildHeadlessLaunchArgs`, the
//! `PUPPETEER_EXECUTABLE_PATH` override and the `PUPPETEER_PROXY*` flags). Chrome for Testing
//! downloads and the stealth patches are not ported.
use std::path::{Path, PathBuf};

/// Environment variables naming the browser executable, in precedence order.
pub const EXECUTABLE_ENV: &[&str] = &[
    "MAHO_BROWSER_PATH",
    "PUPPETEER_EXECUTABLE_PATH",
    "CHROME_PATH",
];

/// Default viewport (oh-my-pi `DEFAULT_VIEWPORT`).
pub const DEFAULT_VIEWPORT: (u32, u32) = (1365, 768);

const UNGOOGLED_CHROMIUM_FLATPAK_ID: &str = "io.github.ungoogled_software.ungoogled_chromium";

fn which(name: &str, path_var: Option<&str>) -> Option<PathBuf> {
    let path_var = path_var?;
    std::env::split_paths(path_var)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable_file(candidate))
}

/// Whether `path` is a regular file this process may execute.
pub fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// System Chrome/Chromium/Edge locations for `platform` (`"macos"`, `"linux"`, `"windows"`).
pub fn system_candidates(
    platform: &str,
    home: &Path,
    path_var: Option<&str>,
    env: &dyn Fn(&str) -> Option<String>,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    match platform {
        "macos" => {
            for root in [PathBuf::from("/Applications"), home.join("Applications")] {
                for app in [
                    "Google Chrome.app/Contents/MacOS/Google Chrome",
                    "Google Chrome Beta.app/Contents/MacOS/Google Chrome Beta",
                    "Google Chrome Dev.app/Contents/MacOS/Google Chrome Dev",
                    "Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
                    "Chromium.app/Contents/MacOS/Chromium",
                    "Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
                ] {
                    candidates.push(root.join(app));
                }
            }
        }
        "linux" => {
            for name in [
                "google-chrome-stable",
                "google-chrome",
                "chromium",
                "chromium-browser",
                "chrome",
            ] {
                candidates.extend(which(name, path_var));
            }
            for path in [
                "/usr/bin/google-chrome-stable",
                "/usr/bin/google-chrome",
                "/usr/bin/chromium",
                "/usr/bin/chromium-browser",
                "/snap/bin/chromium",
                "/var/lib/flatpak/exports/bin/com.google.Chrome",
                "/var/lib/flatpak/exports/bin/org.chromium.Chromium",
            ] {
                candidates.push(PathBuf::from(path));
            }
            if Path::new("/etc/NIXOS").exists() {
                candidates.push(home.join(".nix-profile/bin/chromium"));
                candidates.push(PathBuf::from("/run/current-system/sw/bin/chromium"));
            }
            for name in ["ungoogled-chromium", "ungoogled-chromium-browser"] {
                candidates.extend(which(name, path_var));
            }
            candidates.push(PathBuf::from("/usr/bin/ungoogled-chromium"));
            candidates.push(PathBuf::from("/usr/bin/ungoogled-chromium-browser"));
            candidates.push(
                Path::new("/var/lib/flatpak/exports/bin").join(UNGOOGLED_CHROMIUM_FLATPAK_ID),
            );
            candidates.push(
                home.join(".local/share/flatpak/exports/bin")
                    .join(UNGOOGLED_CHROMIUM_FLATPAK_ID),
            );
        }
        "windows" => {
            let program_files =
                env("ProgramFiles").unwrap_or_else(|| "C:\\Program Files".to_owned());
            let program_files_x86 =
                env("ProgramFiles(x86)").unwrap_or_else(|| "C:\\Program Files (x86)".to_owned());
            let local_app_data = env("LOCALAPPDATA").map_or_else(
                || home.join("AppData").join("Local"),
                PathBuf::from,
            );
            let program_files = PathBuf::from(program_files);
            let program_files_x86 = PathBuf::from(program_files_x86);
            candidates.push(program_files.join("Google\\Chrome\\Application\\chrome.exe"));
            candidates.push(program_files_x86.join("Google\\Chrome\\Application\\chrome.exe"));
            candidates.push(local_app_data.join("Google\\Chrome\\Application\\chrome.exe"));
            candidates.push(program_files.join("Chromium\\Application\\chrome.exe"));
            candidates.push(local_app_data.join("Chromium\\Application\\chrome.exe"));
            candidates.push(program_files.join("Microsoft\\Edge\\Application\\msedge.exe"));
            candidates.push(program_files_x86.join("Microsoft\\Edge\\Application\\msedge.exe"));
        }
        _ => {}
    }
    candidates
}

fn current_platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(windows) {
        "windows"
    } else {
        "linux"
    }
}

/// The executable named by [`EXECUTABLE_ENV`], else the first system candidate that exists.
pub fn find_executable(env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    for key in EXECUTABLE_ENV {
        if let Some(value) = env(key).filter(|value| !value.trim().is_empty()) {
            return Some(PathBuf::from(value.trim()));
        }
    }
    let home = env("HOME")
        .or_else(|| env("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default();
    let path_var = env("PATH");
    let mut seen = std::collections::HashSet::new();
    system_candidates(current_platform(), &home, path_var.as_deref(), env)
        .into_iter()
        .filter(|candidate| seen.insert(candidate.clone()))
        .find(|candidate| is_executable_file(candidate))
}

fn truthy(value: Option<String>) -> bool {
    value.is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

/// Per-launch features.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaunchFeatures {
    pub ignore_https_errors: bool,
    pub allow_file_access: bool,
}

/// Base Chromium argv (oh-my-pi `buildHeadlessLaunchArgs` without the WebMCP feature flag) plus
/// the profile, headless and first-run switches the puppeteer launcher adds on its own.
pub fn launch_args(
    viewport: (u32, u32),
    headless: bool,
    user_data_dir: &Path,
    features: &LaunchFeatures,
    env: &dyn Fn(&str) -> Option<String>,
) -> Vec<String> {
    let mut args = vec![
        "--no-sandbox".to_owned(),
        "--disable-setuid-sandbox".to_owned(),
        "--disable-blink-features=AutomationControlled".to_owned(),
        "--hide-scrollbars".to_owned(),
        format!("--window-size={},{}", viewport.0, viewport.1),
        format!("--user-data-dir={}", user_data_dir.display()),
        "--no-first-run".to_owned(),
        "--no-default-browser-check".to_owned(),
        "--disable-background-networking".to_owned(),
        "--disable-dev-shm-usage".to_owned(),
        "--mute-audio".to_owned(),
    ];
    if headless {
        args.push("--headless=new".to_owned());
    }
    if features.ignore_https_errors {
        args.push("--ignore-certificate-errors".to_owned());
    }
    if features.allow_file_access {
        args.push("--allow-file-access-from-files".to_owned());
    }
    if let Some(proxy) = env("PUPPETEER_PROXY").filter(|proxy| !proxy.is_empty()) {
        args.push(format!("--proxy-server={proxy}"));
        if truthy(env("PUPPETEER_PROXY_BYPASS_LOOPBACK")) {
            args.push("--proxy-bypass-list=<-loopback>".to_owned());
        }
    }
    if truthy(env("PUPPETEER_PROXY_IGNORE_CERT_ERRORS"))
        && !args.iter().any(|arg| arg == "--ignore-certificate-errors")
    {
        args.push("--ignore-certificate-errors".to_owned());
    }
    args.push("about:blank".to_owned());
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn env_override_wins_in_order() {
        let env = |key: &str| match key {
            "PUPPETEER_EXECUTABLE_PATH" => Some("/opt/b".to_owned()),
            "CHROME_PATH" => Some("/opt/c".to_owned()),
            _ => None,
        };
        assert_eq!(find_executable(&env), Some(PathBuf::from("/opt/b")));
        let env = |key: &str| (key == "MAHO_BROWSER_PATH").then(|| " /opt/a ".to_owned());
        assert_eq!(find_executable(&env), Some(PathBuf::from("/opt/a")));
    }

    #[test]
    fn candidates_cover_each_platform() {
        let home = Path::new("/home/u");
        let mac = system_candidates("macos", home, None, &no_env);
        assert!(mac.contains(&PathBuf::from(
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
        )));
        assert!(mac.contains(&home.join("Applications/Chromium.app/Contents/MacOS/Chromium")));
        let linux = system_candidates("linux", home, None, &no_env);
        assert_eq!(linux.first(), Some(&PathBuf::from("/usr/bin/google-chrome-stable")));
        assert!(linux.contains(&PathBuf::from("/snap/bin/chromium")));
        let windows = system_candidates("windows", home, None, &|key: &str| {
            (key == "ProgramFiles").then(|| "D:\\PF".to_owned())
        });
        assert!(windows.contains(&PathBuf::from("D:\\PF").join("Google\\Chrome\\Application\\chrome.exe")));
        assert!(system_candidates("plan9", home, None, &no_env).is_empty());
    }

    #[test]
    fn path_lookup_finds_executables() {
        let Ok(dir) = tempfile::tempdir() else { return };
        let exe = dir.path().join("chromium");
        if std::fs::write(&exe, "#!/bin/sh\n").is_err() {
            return;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755));
        }
        let path_var = dir.path().display().to_string();
        let found = system_candidates("linux", Path::new("/nonexistent"), Some(&path_var), &no_env);
        assert_eq!(found.first(), Some(&exe));
    }

    #[test]
    fn launch_args_follow_features_and_proxy_env() {
        let env = |key: &str| match key {
            "PUPPETEER_PROXY" => Some("http://proxy:8080".to_owned()),
            "PUPPETEER_PROXY_BYPASS_LOOPBACK" => Some("yes".to_owned()),
            "PUPPETEER_PROXY_IGNORE_CERT_ERRORS" => Some("1".to_owned()),
            _ => None,
        };
        let args = launch_args(
            (800, 600),
            true,
            Path::new("/tmp/p"),
            &LaunchFeatures {
                ignore_https_errors: true,
                allow_file_access: true,
            },
            &env,
        );
        assert!(args.contains(&"--window-size=800,600".to_owned()));
        assert!(args.contains(&"--user-data-dir=/tmp/p".to_owned()));
        assert!(args.contains(&"--headless=new".to_owned()));
        assert!(args.contains(&"--allow-file-access-from-files".to_owned()));
        assert!(args.contains(&"--proxy-server=http://proxy:8080".to_owned()));
        assert!(args.contains(&"--proxy-bypass-list=<-loopback>".to_owned()));
        assert_eq!(
            args.iter()
                .filter(|arg| *arg == "--ignore-certificate-errors")
                .count(),
            1
        );
        assert_eq!(args.last().map(String::as_str), Some("about:blank"));
        let headed = launch_args(DEFAULT_VIEWPORT, false, Path::new("/p"), &LaunchFeatures::default(), &no_env);
        assert!(!headed.iter().any(|arg| arg.starts_with("--headless")));
    }
}
