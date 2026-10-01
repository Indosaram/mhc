//! Port of senpi packages/coding-agent/src/core/package-identity.ts.
//!
//! Nearest package.json name plus the resource path relative to that package root, so two physical
//! copies of one package (global install vs worktree checkout) share one identity.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageIdentity {
    pub key: String,
    pub package_name: String,
}

pub fn find_nearest_package_identity(resource_path: &str) -> Option<PackageIdentity> {
    if resource_path.starts_with('<') {
        return None;
    }
    let normalized_resource_path =
        std::fs::canonicalize(resource_path).unwrap_or_else(|_| PathBuf::from(resource_path));
    let mut current_path = PathBuf::from(resource_path);
    match std::fs::metadata(&current_path) {
        Ok(metadata) if metadata.is_dir() => {}
        _ => {
            current_path = current_path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("/"));
        }
    }

    loop {
        let package_json_path = current_path.join("package.json");
        if package_json_path.exists() {
            let parsed: serde_json::Value = std::fs::read_to_string(&package_json_path)
                .ok()
                .and_then(|content| serde_json::from_str(&content).ok())?;
            let name = parsed.get("name").and_then(serde_json::Value::as_str).filter(|name| !name.is_empty())?;
            let relative = normalized_resource_path
                .strip_prefix(&current_path)
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_else(|_| normalized_resource_path.to_string_lossy().into_owned());
            return Some(PackageIdentity { key: format!("{name}:{relative}"), package_name: name.to_owned() });
        }
        let parent = current_path.parent().map(Path::to_path_buf)?;
        if parent == current_path {
            return None;
        }
        current_path = parent;
    }
}

/// First path per identity wins (CLI before settings before discovery); unpackaged paths pass through.
pub fn dedupe_paths_by_package_identity(paths: &[String]) -> Vec<String> {
    let mut deduped = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for path in paths {
        if let Some(identity) = find_nearest_package_identity(path)
            && !seen.insert(identity.key)
        {
            continue;
        }
        deduped.push(path.clone());
    }
    deduped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dash_prefixed_path_has_no_identity() {
        assert!(find_nearest_package_identity("<runtime>").is_none());
    }

    #[test]
    fn finds_the_nearest_package_json_and_relative_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let pkg = tmp.path().join("pkg");
        std::fs::create_dir_all(pkg.join("src/skills")).expect("mkdir");
        std::fs::write(pkg.join("package.json"), r#"{"name":"@scope/pkg"}"#).expect("write");
        let resource = pkg.join("src/skills/a.md");
        std::fs::write(&resource, "x").expect("write");
        let identity = find_nearest_package_identity(&resource.to_string_lossy()).expect("identity");
        assert_eq!(identity.package_name, "@scope/pkg");
        assert!(identity.key.starts_with("@scope/pkg:"));
        assert!(identity.key.ends_with("src/skills/a.md"));
    }

    #[test]
    fn a_package_without_a_name_is_ignored() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("package.json"), r#"{"version":"1.0.0"}"#).expect("write");
        assert!(find_nearest_package_identity(&tmp.path().to_string_lossy()).is_none());
    }

    #[test]
    fn dedupe_keeps_the_first_path_per_identity() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let pkg = tmp.path().join("pkg");
        std::fs::create_dir_all(pkg.join("a")).expect("mkdir");
        std::fs::write(pkg.join("package.json"), r#"{"name":"p"}"#).expect("write");
        let first = pkg.join("a/x.md").to_string_lossy().into_owned();
        let second = pkg.join("a/y.md").to_string_lossy().into_owned();
        std::fs::write(&first, "1").expect("write");
        std::fs::write(&second, "2").expect("write");
        let deduped = dedupe_paths_by_package_identity(&[first.clone(), second.clone(), first.clone()]);
        assert_eq!(deduped.len(), 2);
        assert_eq!(deduped[0], first);
    }
}
