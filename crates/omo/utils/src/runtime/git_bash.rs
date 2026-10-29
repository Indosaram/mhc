use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

pub const GIT_BASH_ENV_KEY: &str = "OMO_CODEX_GIT_BASH_PATH";
pub const WINGET_INSTALL_ARGS: [&str; 6] =
    ["install", "--id", "Git.Git", "-e", "--source", "winget"];

const PROGRAM_FILES_GIT_BASH: &str = "C:\\Program Files\\Git\\bin\\bash.exe";
const PROGRAM_FILES_X86_GIT_BASH: &str = "C:\\Program Files (x86)\\Git\\bin\\bash.exe";
const NON_GIT_BASH_LAUNCHER_DIR_SEGMENTS: [&str; 2] =
    ["\\windows\\system32\\", "\\microsoft\\windowsapps\\"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitBashSource {
    NotRequired,
    Env,
    ProgramFiles,
    ProgramFilesX86,
    Path,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitBashResolution {
    Found {
        path: Option<String>,
        source: GitBashSource,
        checked_paths: Vec<String>,
    },
    Missing {
        checked_paths: Vec<String>,
        install_hint: String,
    },
}

/// Inputs for [`resolve_git_bash`]; `platform` uses Node names (`win32`, `darwin`, ...).
pub struct GitBashResolverInput<'a> {
    pub platform: &'a str,
    pub env: &'a HashMap<String, String>,
    pub exists: &'a dyn Fn(&str) -> bool,
    pub where_bash: &'a dyn Fn() -> Vec<String>,
}

fn missing(checked_paths: Vec<String>) -> GitBashResolution {
    GitBashResolution::Missing {
        checked_paths,
        install_hint: [
            "Git Bash is required on native Windows.".to_string(),
            "Install it with: winget install --id Git.Git -e --source winget".to_string(),
            format!("For a custom install, set {GIT_BASH_ENV_KEY}=C:\\path\\to\\bash.exe"),
        ]
        .join("\n"),
    }
}

fn is_bash_exe_path(path: &str) -> bool {
    path.to_lowercase().ends_with("bash.exe")
}

fn is_known_non_git_bash_launcher(path: &str) -> bool {
    let normalized = path.replace('/', "\\").to_lowercase();
    NON_GIT_BASH_LAUNCHER_DIR_SEGMENTS
        .iter()
        .any(|segment| normalized.contains(segment))
}

pub fn resolve_git_bash(input: &GitBashResolverInput<'_>) -> GitBashResolution {
    if input.platform != "win32" {
        return GitBashResolution::Found {
            path: None,
            source: GitBashSource::NotRequired,
            checked_paths: Vec::new(),
        };
    }
    let mut checked_paths = Vec::new();
    let env_path = input
        .env
        .get(GIT_BASH_ENV_KEY)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty());
    if let Some(env_path) = env_path {
        checked_paths.push(env_path.to_string());
        if is_bash_exe_path(env_path) && (input.exists)(env_path) {
            return GitBashResolution::Found {
                path: Some(env_path.to_string()),
                source: GitBashSource::Env,
                checked_paths,
            };
        }
        return missing(checked_paths);
    }
    for (path, source) in [
        (PROGRAM_FILES_GIT_BASH, GitBashSource::ProgramFiles),
        (PROGRAM_FILES_X86_GIT_BASH, GitBashSource::ProgramFilesX86),
    ] {
        checked_paths.push(path.to_string());
        if (input.exists)(path) {
            return GitBashResolution::Found {
                path: Some(path.to_string()),
                source,
                checked_paths,
            };
        }
    }
    for candidate in (input.where_bash)() {
        let candidate = candidate.trim();
        if candidate.is_empty() {
            continue;
        }
        checked_paths.push(candidate.to_string());
        if is_known_non_git_bash_launcher(candidate) {
            continue;
        }
        if is_bash_exe_path(candidate) && (input.exists)(candidate) {
            return GitBashResolution::Found {
                path: Some(candidate.to_string()),
                source: GitBashSource::Path,
                checked_paths,
            };
        }
    }
    missing(checked_paths)
}

fn where_bash() -> Vec<String> {
    Command::new("where")
        .arg("bash")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

pub fn resolve_git_bash_for_current_process() -> GitBashResolution {
    let platform = if cfg!(windows) {
        "win32"
    } else {
        std::env::consts::OS
    };
    let env: HashMap<String, String> = std::env::vars().collect();
    resolve_git_bash(&GitBashResolverInput {
        platform,
        env: &env,
        exists: &|path| Path::new(path).exists(),
        where_bash: &where_bash,
    })
}
