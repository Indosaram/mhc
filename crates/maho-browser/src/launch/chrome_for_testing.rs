//! Chrome for Testing: the managed Chromium downloaded on first use.
//!
//! Port of oh-my-pi `packages/coding-agent/src/tools/browser/launch.ts` (`ensureChromiumExecutable`,
//! `ensureSignInChromiumExecutable`, `ensureManagedChromium`) and of the parts of
//! `@puppeteer/browsers` it calls (`detectBrowserPlatform`, `computeExecutablePath` /
//! `installationDir` cache layout `<cache>/chrome/<platform>-<buildId>/`, `resolveDownloadUrl`,
//! `relativeExecutablePath`, `install`: download the archive next to the install dir, unpack,
//! remove the archive). The build is puppeteer-core 25.3.0's `PUPPETEER_REVISIONS.chrome`, the
//! version oh-my-pi pins.
//!
//! PORT-DIVERGENCE: the archive is fetched with the system `curl` and unpacked with `unzip`
//! (`tar -xf` on Windows, whose bsdtar reads zip) instead of Node streams and `extract-zip`, so
//! the crate takes no HTTP or zip dependency; both steps are injectable ([`Installer`]) for tests.
//! The cache lives under `$MAHO_BROWSER_CACHE`, else `$XDG_CACHE_HOME/maho/puppeteer`, else
//! `~/.cache/maho/puppeteer` (oh-my-pi: `~/.omp/puppeteer` via `getPuppeteerDir`). macOS prefers
//! the managed browser over a system Chrome because a daemon launched from the user's
//! `Google Chrome.app` shares its LaunchServices identity and can swallow link clicks (#8673).
use super::{EXECUTABLE_ENV, find_system_executable, stealth::is_microsoft_edge_executable};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, PoisonError};

/// The managed browser once installed (`chromiumExecutablePromise`): one download per process.
static MANAGED_EXECUTABLE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// The executable chosen after the managed install was tried (`chromiumExecutablePromise`):
/// the managed path or, on macOS after a failed install, the system Chrome fallback.
static RESOLVED_EXECUTABLE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Runs `install` at most once successfully per `slot`: concurrent callers wait on the lock
/// while the first one downloads, then reuse its path. A failure is not cached, so the next
/// call retries (oh-my-pi clears its promise on failure).
fn memoised(
    slot: &Mutex<Option<PathBuf>>,
    install: impl FnOnce() -> Result<Option<PathBuf>, String>,
) -> Result<Option<PathBuf>, String> {
    let mut cached = slot.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(path) = cached.as_ref() {
        return Ok(Some(path.clone()));
    }
    let installed = install()?;
    cached.clone_from(&installed);
    Ok(installed)
}

/// puppeteer-core 25.3.0 `PUPPETEER_REVISIONS.chrome`.
pub const CHROME_BUILD_ID: &str = "150.0.7871.24";
/// `resolveDownloadUrl` default base.
pub const DOWNLOAD_BASE_URL: &str = "https://storage.googleapis.com/chrome-for-testing-public";

/// `BrowserPlatform`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserPlatform {
    Linux,
    LinuxArm,
    Mac,
    MacArm,
    Win32,
    Win64,
}

impl BrowserPlatform {
    /// The cache directory name part (`BrowserPlatform` value).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::LinuxArm => "linux_arm",
            Self::Mac => "mac",
            Self::MacArm => "mac_arm",
            Self::Win32 => "win32",
            Self::Win64 => "win64",
        }
    }

    /// `folder(platform)` in the download path.
    fn folder(self) -> &'static str {
        match self {
            Self::Linux | Self::LinuxArm => "linux64",
            Self::MacArm => "mac-arm64",
            Self::Mac => "mac-x64",
            Self::Win32 => "win32",
            Self::Win64 => "win64",
        }
    }

}

/// `detectBrowserPlatform` for `(os, arch)` as Rust names them; `None` elsewhere.
pub fn detect_browser_platform(os: &str, arch: &str) -> Option<BrowserPlatform> {
    match os {
        "macos" => Some(if arch == "aarch64" {
            BrowserPlatform::MacArm
        } else {
            BrowserPlatform::Mac
        }),
        "linux" => Some(if arch == "aarch64" {
            BrowserPlatform::LinuxArm
        } else {
            BrowserPlatform::Linux
        }),
        // Windows 11 on ARM runs the x64 build under emulation.
        "windows" => Some(if arch == "x86_64" || arch == "aarch64" {
            BrowserPlatform::Win64
        } else {
            BrowserPlatform::Win32
        }),
        _ => None,
    }
}

/// `resolveDownloadUrl(platform, buildId)`.
pub fn download_url(platform: BrowserPlatform, build_id: &str) -> String {
    let folder = platform.folder();
    format!("{DOWNLOAD_BASE_URL}/{build_id}/{folder}/chrome-{folder}.zip")
}

/// `relativeExecutablePath(platform)`.
pub fn relative_executable_path(platform: BrowserPlatform) -> PathBuf {
    let folder = format!("chrome-{}", platform.folder());
    match platform {
        BrowserPlatform::Mac | BrowserPlatform::MacArm => Path::new(&folder)
            .join("Google Chrome for Testing.app")
            .join("Contents")
            .join("MacOS")
            .join("Google Chrome for Testing"),
        BrowserPlatform::Linux | BrowserPlatform::LinuxArm => Path::new(&folder).join("chrome"),
        BrowserPlatform::Win32 | BrowserPlatform::Win64 => Path::new(&folder).join("chrome.exe"),
    }
}

/// `Cache.installationDir`: `<cache>/chrome/<platform>-<buildId>`.
pub fn installation_dir(cache_dir: &Path, platform: BrowserPlatform, build_id: &str) -> PathBuf {
    cache_dir
        .join("chrome")
        .join(format!("{}-{build_id}", platform.as_str()))
}

/// `computeExecutablePath`.
pub fn compute_executable_path(
    cache_dir: &Path,
    platform: BrowserPlatform,
    build_id: &str,
) -> PathBuf {
    installation_dir(cache_dir, platform, build_id).join(relative_executable_path(platform))
}

/// Where managed browsers are cached.
pub fn cache_dir(env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let set = |key: &str| env(key).filter(|value| !value.trim().is_empty());
    if let Some(dir) = set("MAHO_BROWSER_CACHE") {
        return Some(PathBuf::from(dir));
    }
    if let Some(xdg) = set("XDG_CACHE_HOME") {
        return Some(Path::new(&xdg).join("maho").join("puppeteer"));
    }
    let home = set("HOME").or_else(|| set("USERPROFILE"))?;
    Some(
        Path::new(&home)
            .join(".cache")
            .join("maho")
            .join("puppeteer"),
    )
}

/// Downloads `url` to the file at the second path.
pub type Fetch<'a> = &'a dyn Fn(&str, &Path) -> Result<(), String>;
/// Unpacks the archive at the first path into the directory at the second.
pub type Unpack<'a> = &'a dyn Fn(&Path, &Path) -> Result<(), String>;

fn run(command: &mut Command, what: &str) -> Result<(), String> {
    let output = command
        .output()
        .map_err(|error| format!("cannot run {what}: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{what} failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// `downloadFile` over the system `curl`.
pub fn curl_fetch(url: &str, archive: &Path) -> Result<(), String> {
    run(
        Command::new("curl")
            .args([
                "--fail",
                "--location",
                "--silent",
                "--show-error",
                "--output",
            ])
            .arg(archive)
            .arg(url),
        "curl",
    )
}

/// `unpackArchive` for zip archives.
pub fn system_unzip(archive: &Path, dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    if cfg!(windows) {
        run(
            Command::new("tar")
                .arg("-xf")
                .arg(archive)
                .arg("-C")
                .arg(dir),
            "tar",
        )
    } else {
        run(
            Command::new("unzip")
                .args(["-q", "-o"])
                .arg(archive)
                .arg("-d")
                .arg(dir),
            "unzip",
        )
    }
}

/// How a managed browser is installed.
pub struct Installer<'a> {
    pub cache_dir: PathBuf,
    pub platform: BrowserPlatform,
    pub build_id: String,
    pub fetch: Fetch<'a>,
    pub unpack: Unpack<'a>,
}

impl Installer<'_> {
    pub fn executable_path(&self) -> PathBuf {
        compute_executable_path(&self.cache_dir, self.platform, &self.build_id)
    }

    /// `install`: returns the executable, downloading and unpacking it when it is missing.
    pub fn install(&self) -> Result<PathBuf, String> {
        let executable = self.executable_path();
        let output = installation_dir(&self.cache_dir, self.platform, &self.build_id);
        if output.exists() {
            if !executable.exists() {
                return Err(format!(
                    "The browser folder ({}) exists but the executable ({}) is missing",
                    output.display(),
                    executable.display()
                ));
            }
            return Ok(executable);
        }
        let url = download_url(self.platform, &self.build_id);
        let browser_root = self.cache_dir.join("chrome");
        std::fs::create_dir_all(&browser_root)
            .map_err(|error| format!("{}: {error}", browser_root.display()))?;
        let archive = browser_root.join(url.rsplit('/').next().unwrap_or("chrome.zip"));
        let outcome = (|| -> Result<PathBuf, String> {
            if !archive.exists() {
                (self.fetch)(&url, &archive)?;
            }
            if let Err(error) = (self.unpack)(&archive, &output) {
                let _ = std::fs::remove_dir_all(&output);
                return Err(error);
            }
            if !executable.exists() {
                return Err(format!(
                    "Chrome for Testing archive did not contain {}",
                    executable.display()
                ));
            }
            Ok(executable.clone())
        })();
        let _ = std::fs::remove_file(&archive);
        outcome
    }
}

/// Which window the executable is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    /// The automation browser.
    Browser,
    /// An interactive sign-in window: Google rejects automation-controlled Edge, so Edge is
    /// replaced with Chrome for Testing (`ensureSignInChromiumExecutable`).
    SignIn,
}

/// Inputs of [`ensure_chromium_executable`].
pub struct Resolve<'a> {
    pub env: &'a dyn Fn(&str) -> Option<String>,
    /// `"macos"`, `"linux"` or `"windows"`.
    pub os: &'a str,
    /// The system browser lookup (`resolveSystemChromium`).
    pub system: &'a dyn Fn() -> Option<PathBuf>,
    /// Installs the managed browser (`ensureManagedChromium`); `Ok(None)` when the platform is
    /// unknown.
    pub managed: &'a dyn Fn() -> Result<Option<PathBuf>, String>,
    /// Caches the result of the managed install or its macOS system fallback
    /// (`chromiumExecutablePromise`), so a failed download is not retried once a fallback
    /// was chosen. Errors are not cached.
    pub resolved: &'a Mutex<Option<PathBuf>>,
}

fn env_override(env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    EXECUTABLE_ENV
        .iter()
        .find_map(|key| env(key).filter(|value| !value.trim().is_empty()))
        .map(|value| PathBuf::from(value.trim()))
}

/// `ensureChromiumExecutable` (and the sign-in variant): an explicit executable variable wins;
/// macOS prefers Chrome for Testing and falls back to a system Chrome; elsewhere a system
/// browser is used and Chrome for Testing is downloaded only when none exists.
pub fn ensure_chromium_executable(
    resolve: &Resolve<'_>,
    purpose: Purpose,
) -> Result<Option<PathBuf>, String> {
    let chosen = resolve_executable(resolve)?;
    if purpose == Purpose::Browser
        || !is_microsoft_edge_executable(chosen.as_ref().and_then(|path| path.to_str()))
    {
        return Ok(chosen);
    }
    (resolve.managed)().map_err(|error| {
        format!(
            "Browser sign-in cannot use Microsoft Edge and Chrome for Testing is unavailable: {error}. Set PUPPETEER_EXECUTABLE_PATH to a Chrome or Chromium binary."
        )
    })
}

fn resolve_executable(resolve: &Resolve<'_>) -> Result<Option<PathBuf>, String> {
    if let Some(path) = env_override(resolve.env) {
        return Ok(Some(path));
    }
    let prefer_managed = resolve.os == "macos";
    if !prefer_managed && let Some(system) = (resolve.system)() {
        return Ok(Some(system));
    }
    memoised(resolve.resolved, || match (resolve.managed)() {
        Ok(path) => Ok(path),
        Err(error) => {
            if prefer_managed && let Some(system) = (resolve.system)() {
                eprintln!(
                    "Chrome for Testing unavailable ({error}); falling back to the system Chrome bundle {}. On macOS this can let the headless browser daemon capture your link clicks (#8673). Set PUPPETEER_EXECUTABLE_PATH to a dedicated Chromium to avoid this.",
                    system.display()
                );
                return Ok(Some(system));
            }
            Err(format!(
                "Failed to install Chromium: {error}. Set PUPPETEER_EXECUTABLE_PATH to use an existing Chrome/Chromium binary, or install one manually."
            ))
        }
    })
}

fn current_os() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(windows) {
        "windows"
    } else {
        "linux"
    }
}

/// [`ensure_chromium_executable`] with the process environment, the system browser lookup and a
/// `curl` + `unzip` install into [`cache_dir`].
pub fn ensure_chromium_executable_from_env(purpose: Purpose) -> Result<Option<PathBuf>, String> {
    let env = |key: &str| std::env::var(key).ok();
    let os = current_os();
    let system = || find_system_executable(&env);
    let managed = || memoised(&MANAGED_EXECUTABLE, install_managed);
    ensure_chromium_executable(
        &Resolve {
            env: &env,
            os,
            system: &system,
            managed: &managed,
            resolved: &RESOLVED_EXECUTABLE,
        },
        purpose,
    )
}

/// `ensureManagedChromium`: Chrome for Testing in [`cache_dir`], downloaded when missing.
fn install_managed() -> Result<Option<PathBuf>, String> {
    let env = |key: &str| std::env::var(key).ok();
    let Some(platform) = detect_browser_platform(current_os(), std::env::consts::ARCH) else {
        return Ok(None);
    };
    let cache = cache_dir(&env).ok_or("cannot determine the browser cache directory")?;
    let installer = Installer {
        cache_dir: cache,
        platform,
        build_id: CHROME_BUILD_ID.to_owned(),
        fetch: &curl_fetch,
        unpack: &system_unzip,
    };
    if !installer.executable_path().exists() {
        eprintln!(
            "Downloading Chrome for Testing {CHROME_BUILD_ID} ({}) into {} (first browser use)",
            platform.as_str(),
            installer.cache_dir.display()
        );
    }
    installer.install().map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn platforms_urls_and_cache_paths_follow_puppeteer_browsers() {
        assert_eq!(
            detect_browser_platform("linux", "x86_64"),
            Some(BrowserPlatform::Linux)
        );
        assert_eq!(
            detect_browser_platform("macos", "aarch64"),
            Some(BrowserPlatform::MacArm)
        );
        assert_eq!(
            detect_browser_platform("windows", "x86"),
            Some(BrowserPlatform::Win32)
        );
        assert_eq!(detect_browser_platform("freebsd", "x86_64"), None);
        assert_eq!(
            download_url(BrowserPlatform::Linux, "150.0.7871.24"),
            "https://storage.googleapis.com/chrome-for-testing-public/150.0.7871.24/linux64/chrome-linux64.zip"
        );
        assert_eq!(
            download_url(BrowserPlatform::MacArm, "1.2.3.4"),
            "https://storage.googleapis.com/chrome-for-testing-public/1.2.3.4/mac-arm64/chrome-mac-arm64.zip"
        );
        let cache = Path::new("/c");
        assert_eq!(
            compute_executable_path(cache, BrowserPlatform::Linux, "9.9"),
            Path::new("/c/chrome/linux-9.9/chrome-linux64/chrome")
        );
        assert_eq!(
            compute_executable_path(cache, BrowserPlatform::Mac, "9.9"),
            Path::new(
                "/c/chrome/mac-9.9/chrome-mac-x64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing"
            )
        );
        assert_eq!(
            compute_executable_path(cache, BrowserPlatform::Win64, "9.9"),
            Path::new("/c/chrome/win64-9.9/chrome-win64/chrome.exe")
        );
    }

    #[test]
    fn the_cache_dir_honours_overrides() {
        let env = |key: &str| match key {
            "XDG_CACHE_HOME" => Some("/x".to_owned()),
            "HOME" => Some("/h".to_owned()),
            _ => None,
        };
        assert_eq!(cache_dir(&env), Some(PathBuf::from("/x/maho/puppeteer")));
        let home_only = |key: &str| (key == "HOME").then(|| "/h".to_owned());
        assert_eq!(
            cache_dir(&home_only),
            Some(PathBuf::from("/h/.cache/maho/puppeteer"))
        );
        let explicit = |key: &str| (key == "MAHO_BROWSER_CACHE").then(|| "/m".to_owned());
        assert_eq!(cache_dir(&explicit), Some(PathBuf::from("/m")));
        assert_eq!(cache_dir(&|_: &str| None), None);
    }

    #[test]
    fn install_downloads_unpacks_and_removes_the_archive_once() {
        let Ok(dir) = tempfile::tempdir() else {
            panic!("no tempdir")
        };
        let fetched = Cell::new(0);
        let fetch = |url: &str, archive: &Path| -> Result<(), String> {
            fetched.set(fetched.get() + 1);
            assert!(url.ends_with("/linux64/chrome-linux64.zip"), "{url}");
            std::fs::write(archive, b"zip").map_err(|error| error.to_string())
        };
        let unpack = |archive: &Path, out: &Path| -> Result<(), String> {
            assert!(archive.exists());
            let exe = out.join("chrome-linux64").join("chrome");
            std::fs::create_dir_all(exe.parent().unwrap_or(out)).map_err(|e| e.to_string())?;
            std::fs::write(&exe, b"#!/bin/sh\n").map_err(|e| e.to_string())
        };
        let installer = Installer {
            cache_dir: dir.path().to_path_buf(),
            platform: BrowserPlatform::Linux,
            build_id: "1.0".into(),
            fetch: &fetch,
            unpack: &unpack,
        };
        let installed = installer.install();
        assert_eq!(installed.as_ref().ok(), Some(&installer.executable_path()));
        assert!(!dir.path().join("chrome/chrome-linux64.zip").exists());
        assert_eq!(installer.install().ok(), installed.ok());
        assert_eq!(
            fetched.get(),
            1,
            "an installed browser is not downloaded again"
        );
    }

    #[test]
    fn install_reports_a_folder_without_its_executable_and_cleans_failed_unpacks() {
        let Ok(dir) = tempfile::tempdir() else {
            panic!("no tempdir")
        };
        let fetch = |_: &str, archive: &Path| -> Result<(), String> {
            std::fs::write(archive, b"zip").map_err(|error| error.to_string())
        };
        let broken = |_: &Path, out: &Path| -> Result<(), String> {
            let _ = std::fs::create_dir_all(out);
            Err("unzip failed".into())
        };
        let installer = Installer {
            cache_dir: dir.path().to_path_buf(),
            platform: BrowserPlatform::Linux,
            build_id: "2.0".into(),
            fetch: &fetch,
            unpack: &broken,
        };
        assert_eq!(installer.install().err().as_deref(), Some("unzip failed"));
        assert!(!installation_dir(dir.path(), BrowserPlatform::Linux, "2.0").exists());
        let _ =
            std::fs::create_dir_all(installation_dir(dir.path(), BrowserPlatform::Linux, "2.0"));
        assert!(
            installer
                .install()
                .err()
                .is_some_and(|error| error.contains("exists but the executable"))
        );
    }

    fn resolve_with(
        env: &dyn Fn(&str) -> Option<String>,
        os: &str,
        system: Option<&str>,
        managed: Result<Option<&str>, &str>,
        purpose: Purpose,
    ) -> Result<Option<PathBuf>, String> {
        let system_path = system.map(PathBuf::from);
        let managed_result = managed
            .map(|path| path.map(PathBuf::from))
            .map_err(str::to_owned);
        ensure_chromium_executable(
            &Resolve {
                env,
                os,
                system: &|| system_path.clone(),
                managed: &|| managed_result.clone(),
                resolved: &Mutex::new(None),
            },
            purpose,
        )
    }

    #[test]
    fn the_executable_override_wins_before_a_detected_system_chrome() {
        let env = |key: &str| (key == "PUPPETEER_EXECUTABLE_PATH").then(|| "/o/chrome".to_owned());
        let chosen = resolve_with(
            &env,
            "windows",
            Some("C:/PF/Google/Chrome/Application/chrome.exe"),
            Err("offline"),
            Purpose::Browser,
        );
        assert_eq!(chosen, Ok(Some(PathBuf::from("/o/chrome"))));
    }

    #[test]
    fn macos_prefers_chrome_for_testing_over_a_system_chrome() {
        let none = |_: &str| -> Option<String> { None };
        let system = "/Users/u/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
        assert_eq!(
            resolve_with(
                &none,
                "macos",
                Some(system),
                Ok(Some("/cft")),
                Purpose::Browser
            ),
            Ok(Some(PathBuf::from("/cft")))
        );
        assert_eq!(
            resolve_with(
                &none,
                "macos",
                Some(system),
                Err("offline"),
                Purpose::Browser
            ),
            Ok(Some(PathBuf::from(system)))
        );
        assert_eq!(
            resolve_with(
                &none,
                "linux",
                Some("/usr/bin/chromium"),
                Ok(Some("/cft")),
                Purpose::Browser
            ),
            Ok(Some(PathBuf::from("/usr/bin/chromium")))
        );
        assert_eq!(
            resolve_with(&none, "linux", None, Ok(Some("/cft")), Purpose::Browser),
            Ok(Some(PathBuf::from("/cft")))
        );
        assert!(
            resolve_with(&none, "linux", None, Err("offline"), Purpose::Browser)
                .err()
                .is_some_and(|error| error.starts_with("Failed to install Chromium: offline"))
        );
    }

    #[test]
    fn the_macos_system_fallback_is_cached_after_a_failed_install() {
        let none = |_: &str| -> Option<String> { None };
        let system = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
        let installs = std::sync::atomic::AtomicUsize::new(0);
        let managed = || -> Result<Option<PathBuf>, String> {
            installs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err("offline".into())
        };
        let fallback = Mutex::new(None);
        let mac = |system: Option<&str>, slot: &Mutex<Option<PathBuf>>| {
            let system_path = system.map(PathBuf::from);
            ensure_chromium_executable(
                &Resolve {
                    env: &none,
                    os: "macos",
                    system: &|| system_path.clone(),
                    managed: &managed,
                    resolved: slot,
                },
                Purpose::Browser,
            )
        };
        for _ in 0..3 {
            assert_eq!(
                mac(Some(system), &fallback),
                Ok(Some(PathBuf::from(system)))
            );
        }
        assert_eq!(
            installs.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the fallback is reused, the download is not retried"
        );
        // Without a fallback the failure is not cached: each call retries the install.
        let failing = Mutex::new(None);
        for _ in 0..2 {
            assert!(mac(None, &failing).is_err());
        }
        assert_eq!(installs.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[test]
    fn sign_in_replaces_microsoft_edge_with_chrome_for_testing() {
        let edge = "C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe";
        let env = move |key: &str| (key == "PUPPETEER_EXECUTABLE_PATH").then(|| edge.to_owned());
        assert_eq!(
            resolve_with(&env, "windows", None, Ok(Some("/cft")), Purpose::SignIn),
            Ok(Some(PathBuf::from("/cft")))
        );
        assert_eq!(
            resolve_with(&env, "windows", None, Ok(Some("/cft")), Purpose::Browser),
            Ok(Some(PathBuf::from(edge)))
        );
        assert!(
            resolve_with(&env, "windows", None, Err("offline"), Purpose::SignIn)
                .err()
                .is_some_and(|error| error.contains("cannot use Microsoft Edge"))
        );
    }

    #[test]
    fn the_managed_install_runs_once_and_failures_are_retried() {
        let slot = Mutex::new(None);
        let installs = std::sync::atomic::AtomicUsize::new(0);
        assert_eq!(
            memoised(&slot, || Err("offline".into())),
            Err("offline".to_owned())
        );
        let start = std::sync::Barrier::new(4);
        let results: Vec<_> = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..4)
                .map(|_| {
                    scope.spawn(|| {
                        start.wait();
                        memoised(&slot, || {
                            installs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            Ok(Some(PathBuf::from("/cache/chrome")))
                        })
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|worker| worker.join().ok())
                .collect()
        });
        assert_eq!(installs.load(std::sync::atomic::Ordering::SeqCst), 1);
        for result in results {
            assert_eq!(result, Some(Ok(Some(PathBuf::from("/cache/chrome")))));
        }
    }

    /// Downloads the real Chrome for Testing build (needs network, curl and unzip). Run it with
    /// `MAHO_BROWSER_DOWNLOAD_TEST=1` and `--run-ignored ignored-only`.
    #[test]
    #[ignore = "needs MAHO_BROWSER_DOWNLOAD_TEST"]
    fn real_download_installs_a_runnable_browser() {
        assert!(
            std::env::var_os("MAHO_BROWSER_DOWNLOAD_TEST").is_some(),
            "MAHO_BROWSER_DOWNLOAD_TEST is not set"
        );
        let Some(platform) = detect_browser_platform(current_os(), std::env::consts::ARCH) else {
            panic!("no Chrome for Testing build for this platform");
        };
        let Ok(dir) = tempfile::tempdir() else {
            panic!("no tempdir")
        };
        let installer = Installer {
            cache_dir: dir.path().to_path_buf(),
            platform,
            build_id: CHROME_BUILD_ID.into(),
            fetch: &curl_fetch,
            unpack: &system_unzip,
        };
        let exe = installer
            .install()
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(super::super::is_executable_file(&exe), "{}", exe.display());
    }
}
