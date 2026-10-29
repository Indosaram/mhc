//! Finding the `ast-grep` program (spec §2).

use crate::error::{BINARY_NOT_FOUND, Phase, ToolError};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

/// Environment override of the program path (interface fact).
pub const SG_PATH_ENV: &str = "OMO_AST_GREP_SG_PATH";
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Where candidates come from. [`Toolchain::from_env`] reads the process; tests build one by hand.
#[derive(Clone, Debug, Default)]
pub struct Toolchain {
    /// `OMO_AST_GREP_SG_PATH`, already trimmed; `None` when empty.
    pub override_path: Option<String>,
    /// The user's home directory (for the managed install).
    pub home: Option<PathBuf>,
    /// Directories of `PATH`.
    pub path_dirs: Vec<PathBuf>,
    /// Extra directories searched last (Homebrew locations).
    pub brew_dirs: Vec<PathBuf>,
    /// Node-style platform name (`linux`, `darwin`, `win32`).
    pub platform: String,
    /// Node-style architecture name (`x64`, `arm64`).
    pub arch: String,
}

/// Node-style platform name of this build.
pub fn node_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

/// Node-style architecture name of this build.
pub fn node_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        other => other,
    }
}

/// The platform's standard Homebrew bin directories.
pub fn brew_dirs_for(platform: &str, home: Option<&Path>) -> Vec<PathBuf> {
    match platform {
        "darwin" => vec![
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
        ],
        "linux" => {
            let mut dirs = vec![PathBuf::from("/home/linuxbrew/.linuxbrew/bin")];
            if let Some(home) = home {
                dirs.push(home.join(".linuxbrew").join("bin"));
            }
            dirs
        }
        _ => Vec::new(),
    }
}

impl Toolchain {
    pub fn from_env() -> Self {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from);
        let platform = node_platform().to_owned();
        Self {
            override_path: std::env::var(SG_PATH_ENV)
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty()),
            path_dirs: std::env::var_os("PATH")
                .map(|path| std::env::split_paths(&path).collect())
                .unwrap_or_default(),
            brew_dirs: brew_dirs_for(&platform, home.as_deref()),
            home,
            platform,
            arch: node_arch().to_owned(),
        }
    }

    fn exe_names(&self) -> [&'static str; 2] {
        if self.platform == "win32" {
            ["ast-grep.exe", "sg.exe"]
        } else {
            ["ast-grep", "sg"]
        }
    }

    /// Candidates in order of preference (spec §2).
    pub fn candidates(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Some(path) = &self.override_path {
            out.push(PathBuf::from(path));
        }
        if let Some(home) = &self.home {
            let managed = home
                .join(".omo")
                .join("runtime")
                .join("ast-grep")
                .join(format!("{}-{}", self.platform, self.arch));
            for name in self.exe_names() {
                out.push(managed.join(name));
            }
        }
        for name in self.exe_names() {
            for dir in &self.path_dirs {
                out.push(dir.join(name));
            }
        }
        for name in self.exe_names() {
            for dir in &self.brew_dirs {
                out.push(dir.join(name));
            }
        }
        out
    }

    /// Install hints for the platform.
    pub fn install_hints(&self) -> Vec<String> {
        let mut hints = Vec::new();
        match self.platform.as_str() {
            "win32" => {
                hints.push("scoop install main/ast-grep".to_owned());
                hints.push("winget install ast-grep.ast-grep".to_owned());
                hints.push("choco install ast-grep".to_owned());
            }
            "darwin" | "linux" => hints.push("brew install ast-grep".to_owned()),
            _ => {}
        }
        hints.push("npm install --global @ast-grep/cli".to_owned());
        hints.push("cargo install ast-grep --locked".to_owned());
        hints
    }

    /// The error for "no usable program".
    pub fn not_found_error(&self, tried: &[PathBuf]) -> ToolError {
        ToolError::new(
            BINARY_NOT_FOUND,
            Phase::Preflight,
            format!(
                "The ast-grep program was not found on {} ({}). Install it or set {SG_PATH_ENV}.",
                self.platform, self.arch
            ),
        )
        .with_details(json!({
            "hints": self.install_hints(),
            "platform": self.platform,
            "arch": self.arch,
            "candidates": tried.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>(),
        }))
    }
}

type CacheKey = (String, String, Vec<PathBuf>);
static CACHE: Mutex<Option<(CacheKey, PathBuf)>> = Mutex::new(None);

fn cache_get(key: &CacheKey) -> Option<PathBuf> {
    let guard = CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    guard
        .as_ref()
        .filter(|(cached_key, path)| cached_key == key && path.is_file())
        .map(|(_, path)| path.clone())
}

fn cache_put(key: CacheKey, path: PathBuf) {
    *CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((key, path));
}

/// A candidate passes when it is a non-empty file whose `--version` output names ast-grep.
pub async fn probe(candidate: &Path) -> bool {
    let non_empty = std::fs::metadata(candidate).is_ok_and(|meta| meta.is_file() && meta.len() > 0);
    if !non_empty {
        return false;
    }
    let mut command = tokio::process::Command::new(candidate);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    let Ok(child) = command.spawn() else {
        return false;
    };
    match tokio::time::timeout(PROBE_TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(output)) => {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            text.to_ascii_lowercase().contains("ast-grep")
        }
        _ => false,
    }
}

/// Resolve the program for one call. A success is cached per (platform, arch, candidates).
pub async fn resolve(toolchain: &Toolchain) -> Result<PathBuf, ToolError> {
    let candidates = toolchain.candidates();
    let key = (toolchain.platform.clone(), toolchain.arch.clone(), candidates.clone());
    if let Some(path) = cache_get(&key) {
        return Ok(path);
    }
    for candidate in &candidates {
        if probe(candidate).await {
            cache_put(key, candidate.clone());
            return Ok(candidate.clone());
        }
    }
    Err(toolchain.not_found_error(&candidates))
}
