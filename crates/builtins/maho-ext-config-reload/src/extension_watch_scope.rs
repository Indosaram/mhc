use std::path::{Component, Path, PathBuf};

fn manifest_entry_paths(extensions_dir: &Path, package_name: &str) -> Vec<PathBuf> {
    let absolute = std::path::absolute(extensions_dir).unwrap_or_else(|_| extensions_dir.to_path_buf());
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::ParentDir => { normalized.pop(); },
            Component::CurDir => {},
            other => normalized.push(other.as_os_str()),
        }
    }
    let extensions_dir = normalized.as_path();
    let package = extensions_dir.join(package_name);
    let Ok(content) = std::fs::read_to_string(package.join("package.json")) else { return Vec::new(); };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content) else { return Vec::new(); };
    let Some(entries) = parsed.get("pi").and_then(|pi| pi.get("extensions")).and_then(serde_json::Value::as_array) else { return Vec::new(); };
    entries.iter().filter_map(serde_json::Value::as_str).filter_map(|entry| {
        let joined = package.join(entry);
        let mut resolved = PathBuf::new();
        for component in joined.components() {
            match component {
                Component::Normal(name) => resolved.push(name),
                Component::CurDir => {},
                Component::ParentDir => { resolved.pop(); },
                Component::RootDir | Component::Prefix(_) => resolved.push(component.as_os_str()),
            }
        }
        let relative = resolved.strip_prefix(extensions_dir).ok()?.to_path_buf();
        let first = relative.components().next()?;
        match first {
            Component::Normal(name) if !name.to_string_lossy().starts_with("..") => Some(relative),
            Component::Normal(_) | Component::CurDir | Component::ParentDir | Component::RootDir | Component::Prefix(_) => None,
        }
    }).collect()
}

fn split_path_segments(path: &str) -> Vec<&str> {
    path.split(['/', '\\']).filter(|s| !s.is_empty()).collect()
}

pub fn is_loadable_extension_entry(extensions_dir: &Path, relative_path: &str) -> bool {
    let segments = split_path_segments(relative_path);
    let Some(first) = segments.first() else { return false; };
    if segments.len() == 1 { return first.ends_with(".ts") || first.ends_with(".js"); }
    if segments.len() == 2 && segments.get(1).is_some_and(|name| ["index.ts", "index.js", "package.json"].contains(name)) { return true; }
    let relative: PathBuf = segments.iter().collect();
    manifest_entry_paths(extensions_dir, first).iter().any(|path| path == &relative)
}

pub fn is_scannable_extension_directory(extensions_dir: &Path, relative_path: &str) -> bool {
    let segments = split_path_segments(relative_path);
    let Some(first) = segments.first() else { return true; };
    if segments.len() == 1 { return true; }
    let relative: PathBuf = segments.iter().collect();
    manifest_entry_paths(extensions_dir, first).iter().any(|path| {
        path == &relative || path.starts_with(&relative)
    })
}
