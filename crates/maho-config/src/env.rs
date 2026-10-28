//! Environment handling: the home directory, the account home and profile selection.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Environment map handed to the loader (tests pass their own).
pub type Env = HashMap<String, String>;

/// The process environment as an [`Env`].
pub fn process_env() -> Env {
    std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .collect()
}

fn non_empty<'a>(env: &'a Env, key: &str) -> Option<&'a str> {
    env.get(key)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
}

/// Replaces backslashes with forward slashes (all platforms).
pub fn forward_slashes(path: &str) -> String {
    path.replace('\\', "/")
}

/// Windows: native separators. Rewriting `\` to `/` would turn a verbatim `\\?\C:\...` path into
/// `//?/C:/...`, which `Path` parses as a different prefix, so joined config paths stop comparing
/// equal (component-wise) to the paths the OS and callers hand back.
#[cfg(windows)]
fn absolute_normalised(raw: &str, cwd: &Path) -> PathBuf {
    let path = PathBuf::from(raw);
    let joined = if path.is_absolute() || has_drive_prefix(raw) {
        path
    } else {
        cwd.join(path)
    };
    maho_support::normalize_lexically(&joined)
}

#[cfg(not(windows))]
fn absolute_normalised(raw: &str, cwd: &Path) -> PathBuf {
    let slashed = forward_slashes(raw);
    let path = PathBuf::from(&slashed);
    let joined = if path.is_absolute() || has_drive_prefix(&slashed) {
        path
    } else {
        cwd.join(path)
    };
    let normal = maho_support::normalize_lexically(&joined);
    PathBuf::from(forward_slashes(&normal.to_string_lossy()))
}

pub(crate) fn has_drive_prefix(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// Home directory for every harness-config path: `HOME`, else `USERPROFILE`, else `cwd`.
/// Made absolute and written with forward slashes (native separators on Windows, so verbatim
/// `\\?\` paths survive). `XDG_CONFIG_HOME`/`APPDATA` are never used.
pub fn home_dir_with(env: &Env, cwd: &Path) -> PathBuf {
    let raw = non_empty(env, "HOME")
        .or_else(|| non_empty(env, "USERPROFILE"))
        .map(str::to_owned)
        .unwrap_or_else(|| cwd.to_string_lossy().into_owned());
    absolute_normalised(&raw, cwd)
}

/// [`home_dir_with`] relative to the process working directory.
pub fn home_dir(env: &Env) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    home_dir_with(env, &cwd)
}

/// The operating-system account's home directory (password database on Unix; the process
/// `HOME`/`USERPROFILE` elsewhere). Differs from [`home_dir`] only when `HOME` was overridden.
pub fn account_home() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        if let Ok(Some(user)) = nix::unistd::User::from_uid(nix::unistd::getuid()) {
            return Some(user.dir);
        }
        None
    }
    #[cfg(not(unix))]
    {
        std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
    }
}

/// `<home>/.omo`.
pub fn user_config_dir(env: &Env) -> PathBuf {
    home_dir(env).join(".omo")
}

/// The user configuration file: `omo.jsonc` if present, else an existing `omo.json`, else the
/// (possibly missing) `omo.jsonc` path.
pub fn user_config_path(env: &Env) -> PathBuf {
    let dir = user_config_dir(env);
    let jsonc = dir.join("omo.jsonc");
    let json = dir.join("omo.json");
    if !jsonc.exists() && json.exists() {
        json
    } else {
        jsonc
    }
}

/// The active overlay profile name: `explicit`, else `OMO_PROFILE`, else `OCX_PROFILE`, else the
/// `profiles/<name>` tail of `OPENCODE_CONFIG_DIR`.
pub fn resolve_profile_name(env: &Env, explicit: Option<&str>) -> Option<String> {
    let pick = |value: Option<&str>| {
        value
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };
    pick(explicit)
        .or_else(|| pick(env.get("OMO_PROFILE").map(String::as_str)))
        .or_else(|| pick(env.get("OCX_PROFILE").map(String::as_str)))
        .or_else(|| {
            env.get("OPENCODE_CONFIG_DIR")
                .and_then(|dir| profile_from_config_dir(dir))
        })
}

fn profile_from_config_dir(dir: &str) -> Option<String> {
    let slashed = forward_slashes(dir.trim());
    let trimmed = slashed.trim_end_matches('/');
    let segments: Vec<&str> = trimmed.split('/').collect();
    if segments.len() < 2 {
        return None;
    }
    let name = segments[segments.len() - 1];
    let parent = segments[segments.len() - 2];
    (parent == "profiles" && !name.is_empty()).then(|| name.to_owned())
}
