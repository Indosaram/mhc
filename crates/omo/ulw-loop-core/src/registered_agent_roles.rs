//! Registered agent roles reader for Codex configurations.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use regex::Regex;

fn read_text(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

fn standalone_role_names(agents_dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(agents_dir) else {
        return Vec::new();
    };
    // PERF-DIVERGENCE(omo registered-agent-roles): upstream builds this RegExp per directory scan; this port compiles it once in a LazyLock. Output is identical.
    static RE_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?m)^\s*name\s*=\s*"([^"\n]+)"\s*(?:#.*)?$"#).expect("valid regex"));
    let mut names = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("toml") {
            continue;
        }
        let Some(text) = read_text(&path) else {
            continue;
        };
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if let Some(caps) = RE_NAME.captures(&text) {
            if let Some(m) = caps.get(1) { names.push(m.as_str().to_string()); }
        } else {
            names.push(stem.to_string());
        }
    }
    names
}

fn config_table_role_names(config_path: &Path) -> Vec<String> {
    let Some(text) = read_text(config_path) else {
        return Vec::new();
    };
    // PERF-DIVERGENCE(omo registered-agent-roles): upstream builds this RegExp per config read; this port compiles it once in a LazyLock. Output is identical.
    static RE_TABLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?m)^\s*\[agents\.(?:"([^"\n]+)"|([A-Za-z0-9_-]+))\]\s*(?:#.*)?$"#).expect("valid regex"));
    let mut names = Vec::new();
    for caps in RE_TABLE.captures_iter(&text) {
        if let Some(m) = caps.get(1).or_else(|| caps.get(2)) {
            names.push(m.as_str().to_string());
        }
    }
    names
}

fn codex_dirs(cwd: &Path, codex_home: Option<&str>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let home = match codex_home {
        Some(h) if !h.trim().is_empty() => PathBuf::from(h.trim()),
        // os.homedir(): HOME on unix, USERPROFILE on Windows.
        _ => std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("/home")).join(".codex"),
    };
    dirs.push(home);

    let mut current = cwd.to_path_buf();
    loop {
        dirs.push(current.join(".codex"));
        if !current.pop() {
            break;
        }
    }

    let mut seen = BTreeSet::new();
    let mut unique = Vec::new();
    for d in dirs {
        if seen.insert(d.clone()) {
            unique.push(d);
        }
    }
    unique
}

pub fn registered_agent_roles(cwd: &Path, codex_home: Option<&str>) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for dir in codex_dirs(cwd, codex_home) {
        for name in standalone_role_names(&dir.join("agents")) {
            names.insert(name);
        }
        for name in config_table_role_names(&dir.join("config.toml")) {
            names.insert(name);
        }
    }
    names
}
