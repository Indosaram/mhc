use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use crate::request_context::resolve;

struct LocalBinRule {
    markers: &'static [&'static str],
    bin_dirs: &'static [&'static [&'static str]],
}

const LOCAL_BIN_RULES: &[LocalBinRule] = &[
    LocalBinRule {
        markers: &["package.json", "bun.lock", "bun.lockb", "package-lock.json", "yarn.lock", "pnpm-lock.yaml"],
        bin_dirs: &[&["node_modules", ".bin"]],
    },
    LocalBinRule {
        markers: &[
            "pyproject.toml",
            "ty.toml",
            "requirements.txt",
            "setup.py",
            "setup.cfg",
            "Pipfile",
            "pyrightconfig.json",
            "ruff.toml",
            ".ruff.toml",
        ],
        bin_dirs: &[
            &[".venv", "bin"],
            &[".venv", "Scripts"],
            &["venv", "bin"],
            &["venv", "Scripts"],
            &[".env", "bin"],
            &[".env", "Scripts"],
        ],
    },
    LocalBinRule {
        markers: &["Gemfile", "Gemfile.lock"],
        bin_dirs: &[&["vendor", "bundle", "bin"], &["bin"]],
    },
    LocalBinRule {
        markers: &["go.mod", "go.sum", "go.work"],
        bin_dirs: &[&["bin"]],
    },
];

#[derive(Debug, Clone, Default)]
pub struct ResolveServerBinaryOptions {
    pub stop_at: Option<String>,
    pub platform: Option<&'static str>,
    pub path_ext: Option<String>,
    pub path_env: Option<String>,
}

static PROBE_COUNT: AtomicUsize = AtomicUsize::new(0);
static RESOLUTION_CACHE: Mutex<Option<HashMap<[String; 6], Option<String>>>> = Mutex::new(None);

pub fn reset_server_binary_resolution_cache_for_tests() {
    if let Ok(mut lock) = RESOLUTION_CACHE.lock()
        && let Some(map) = lock.as_mut() {
        map.clear();
    }
    PROBE_COUNT.store(0, Ordering::SeqCst);
}

pub fn server_binary_probe_count_for_tests() -> usize {
    PROBE_COUNT.load(Ordering::SeqCst)
}

fn probe<P: AsRef<Path>>(path: P) -> bool {
    PROBE_COUNT.fetch_add(1, Ordering::SeqCst);
    path.as_ref().exists()
}
fn executable_suffixes(platform: &str, path_ext: Option<&str>) -> Vec<String> {
    if platform != "win32" {
        return vec![String::new()];
    }
    let configured = path_ext
        .map(str::to_string)
        .or_else(|| std::env::var("PATHEXT").ok())
        .unwrap_or_default();
    let system_exts: Vec<String> = configured
        .split(';')
        .filter(|e| !e.is_empty())
        .map(|e| e.to_lowercase())
        .collect();

    let mut res = vec![String::new()];
    for ext in system_exts {
        if !res.contains(&ext) {
            res.push(ext);
        }
    }
    for fallback in [".exe", ".cmd", ".bat", ".ps1"] {
        let s = fallback.to_string();
        if !res.contains(&s) {
            res.push(s);
        }
    }
    res
}

fn probe_with_suffixes(directory: &Path, command: &str, suffixes: &[String]) -> Option<String> {
    for suffix in suffixes {
        let candidate = directory.join(format!("{command}{suffix}"));
        if probe(&candidate) {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

fn resolve_local(
    command: &str,
    working_directory: &str,
    suffixes: &[String],
    stop_at: Option<&str>,
) -> Option<String> {
    let boundary = stop_at.map(|path| PathBuf::from(resolve(path)));
    let mut current = PathBuf::from(resolve(working_directory));

    loop {
        for rule in LOCAL_BIN_RULES {
            let marker_found = rule.markers.iter().any(|m| probe(current.join(m)));
            if !marker_found {
                continue;
            }
            for bin_parts in rule.bin_dirs {
                let mut bin_dir = current.clone();
                for part in *bin_parts {
                    bin_dir.push(part);
                }
                if let Some(found) = probe_with_suffixes(&bin_dir, command, suffixes) {
                    return Some(found);
                }
            }
        }

        if probe(current.join(".git")) {
            return None;
        }
        if let Some(ref b) = boundary
            && &current == b {
            return None;
        }
        if !current.pop() {
            return None;
        }
    }
}

fn resolve_from_path(
    command: &str,
    suffixes: &[String],
    platform: &str,
    path_env_override: Option<&str>,
) -> Option<String> {
    let path_val = match path_env_override {
        Some(val) => val.to_string(),
        None => {
            let mut v = std::env::var("PATH").unwrap_or_default();
            if platform == "win32" && v.is_empty() {
                v = std::env::var("Path").unwrap_or_default();
            }
            v
        }
    };

    let delimiter = if platform == "win32" { ';' } else { ':' };
    for entry in path_val.split(delimiter) {
        if entry.is_empty() {
            continue;
        }
        if let Some(found) = probe_with_suffixes(Path::new(entry), command, suffixes) {
            return Some(found);
        }
    }
    None
}

pub fn resolve_server_binary(
    command: &[String],
    working_directory: Option<&str>,
    options: &ResolveServerBinaryOptions,
) -> Option<String> {
    let cmd = command.first().filter(|c| !c.is_empty())?;

    let platform = options.platform.unwrap_or(if cfg!(windows) { "win32" } else { "linux" });
    let suffixes = executable_suffixes(platform, options.path_ext.as_deref());

    if cmd.contains('/') || cmd.contains('\\') {
        let path = Path::new(cmd);
        let explicit = if path.is_absolute() {
            path.to_path_buf()
        } else {
            let base = working_directory
                .map(PathBuf::from)
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_else(|| PathBuf::from("/"));
            base.join(path)
        };
        return if probe(&explicit) {
            Some(explicit.to_string_lossy().into_owned())
        } else {
            None
        };
    }

    let effective_path = options.path_env.clone().or_else(|| std::env::var("PATH").ok()).unwrap_or_default();
    let cache_key = [
        working_directory.unwrap_or("").to_string(),
        cmd.clone(),
        platform.to_string(),
        options.stop_at.clone().unwrap_or_default(),
        options.path_ext.clone().unwrap_or_default(),
        effective_path,
    ];

    let mut lock = RESOLUTION_CACHE.lock().ok()?;
    let cache = lock.get_or_insert_with(HashMap::new);
    if let Some(hit) = cache.get(&cache_key) {
        return hit.clone();
    }

    let mut resolved = None;
    if let Some(wd) = working_directory.filter(|s| !s.is_empty()) {
        resolved = resolve_local(cmd, wd, &suffixes, options.stop_at.as_deref());
    }
    if resolved.is_none() {
        resolved = resolve_from_path(cmd, &suffixes, platform, options.path_env.as_deref());
    }
    if resolved.is_none() && cmd == "node" {
        resolved = Some("node".to_string());
    }

    cache.insert(cache_key, resolved.clone());
    resolved
}

pub fn is_server_installed_with_options(
    command: &[String],
    working_directory: Option<&str>,
    options: &ResolveServerBinaryOptions,
) -> bool {
    if command.is_empty() {
        return false;
    }
    resolve_server_binary(command, working_directory, options).is_some()
}
pub fn is_server_installed(command: &[String]) -> bool {
    is_server_installed_with_options(command, None, &ResolveServerBinaryOptions::default())
}
