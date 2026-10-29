//! Locating Git Bash's `bash.exe` on Windows (spec §4).

use crate::host::Host;
use serde_json::{Value, json};

/// Path override users already set; when non-blank it is the only candidate.
pub const PATH_ENV: &str = "OMO_CODEX_GIT_BASH_PATH";
/// Neutral alias of [`PATH_ENV`], checked first.
pub const ALIAS_PATH_ENV: &str = "MHC_GIT_BASH_PATH";
pub const PROGRAM_FILES_BASH: &str = r"C:\Program Files\Git\bin\bash.exe";
pub const PROGRAM_FILES_X86_BASH: &str = r"C:\Program Files (x86)\Git\bin\bash.exe";
pub const WINGET_INSTALL: &str = "winget install --id Git.Git -e --source winget";

/// Where a `bash.exe` was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Env,
    ProgramFiles,
    ProgramFilesX86,
    Path,
    NotRequired,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::ProgramFiles => "program-files",
            Self::ProgramFilesX86 => "program-files-x86",
            Self::Path => "path",
            Self::NotRequired => "not-required",
        }
    }
}

/// Outcome of the lookup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    Found {
        /// `None` only for [`Source::NotRequired`].
        path: Option<String>,
        source: Source,
        checked_paths: Vec<String>,
    },
    Missing {
        checked_paths: Vec<String>,
    },
}

impl Resolution {
    pub fn is_found(&self) -> bool {
        matches!(self, Self::Found { .. })
    }

    /// The located `bash.exe`, if any.
    pub fn bash_path(&self) -> Option<&str> {
        match self {
            Self::Found { path, .. } => path.as_deref(),
            Self::Missing { .. } => None,
        }
    }

    /// The `which_bash` result object.
    pub fn to_json(&self) -> Value {
        match self {
            Self::Found {
                path,
                source,
                checked_paths,
            } => {
                let mut object = serde_json::Map::new();
                object.insert("found".into(), Value::Bool(true));
                if let Some(path) = path {
                    object.insert("path".into(), Value::String(path.clone()));
                }
                object.insert("source".into(), Value::String(source.as_str().into()));
                object.insert("checkedPaths".into(), json!(checked_paths));
                Value::Object(object)
            }
            Self::Missing { checked_paths } => json!({
                "found": false,
                "checkedPaths": checked_paths,
                "installHint": install_hint(),
            }),
        }
    }
}

/// How to get a usable `bash.exe`.
pub fn install_hint() -> String {
    format!(
        "No usable Git Bash (bash.exe) was found. Install Git for Windows with `{WINGET_INSTALL}`, \
         or set {PATH_ENV} (or {ALIAS_PATH_ENV}) to the full path of a bash.exe, for example \
         {PROGRAM_FILES_BASH}."
    )
}

fn ends_in_bash_exe(path: &str) -> bool {
    path.to_ascii_lowercase().ends_with("bash.exe")
}

/// WSL launchers that `where bash` may list ahead of Git Bash.
fn is_wsl_launcher(path: &str) -> bool {
    let normalized = path.replace('/', "\\").to_ascii_lowercase();
    normalized.contains("\\windows\\system32\\") || normalized.contains("\\microsoft\\windowsapps\\")
}

fn non_blank(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// Run the lookup of spec §4 against `host`.
pub fn resolve(host: &impl Host) -> Resolution {
    if host.platform() != "win32" {
        return Resolution::Found {
            path: None,
            source: Source::NotRequired,
            checked_paths: Vec::new(),
        };
    }
    let mut checked_paths = Vec::new();

    let override_path =
        non_blank(host.env_var(ALIAS_PATH_ENV)).or_else(|| non_blank(host.env_var(PATH_ENV)));
    if let Some(path) = override_path {
        checked_paths.push(path.clone());
        return if ends_in_bash_exe(&path) && host.file_exists(&path) {
            Resolution::Found {
                path: Some(path),
                source: Source::Env,
                checked_paths,
            }
        } else {
            Resolution::Missing { checked_paths }
        };
    }

    for (path, source) in [
        (PROGRAM_FILES_BASH, Source::ProgramFiles),
        (PROGRAM_FILES_X86_BASH, Source::ProgramFilesX86),
    ] {
        checked_paths.push(path.to_owned());
        if host.file_exists(path) {
            return Resolution::Found {
                path: Some(path.to_owned()),
                source,
                checked_paths,
            };
        }
    }

    let listed = host.where_bash();
    for line in listed.lines() {
        let candidate = line.trim();
        if candidate.is_empty() {
            continue;
        }
        checked_paths.push(candidate.to_owned());
        if is_wsl_launcher(candidate) {
            continue;
        }
        if ends_in_bash_exe(candidate) && host.file_exists(candidate) {
            return Resolution::Found {
                path: Some(candidate.to_owned()),
                source: Source::Path,
                checked_paths,
            };
        }
    }

    Resolution::Missing { checked_paths }
}
