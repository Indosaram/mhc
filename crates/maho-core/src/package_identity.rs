//! Port of senpi packages/coding-agent/src/core/package-identity.ts.
//!
//! Nearest package.json name plus the resource path relative to that package root, so two physical
//! copies of one package (global install vs worktree checkout) share one identity.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageIdentity {
    pub key: String,
    pub package_name: String,
}

/// The nearest package root and its name; `None` when that package.json has no usable name, or none exists.
type NearestPackage = Option<(PathBuf, String)>;

fn read_nearest_package(package_root: &Path) -> NearestPackage {
    let parsed: serde_json::Value = std::fs::read_to_string(package_root.join("package.json"))
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())?;
    let name = parsed
        .get("name")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.is_empty())?;
    Some((package_root.to_path_buf(), name.to_owned()))
}

/// Walks up from `start_dir` to the nearest package.json. `memo` records the answer for every directory
/// walked, so many resources under one folder probe each ancestor once instead of once per resource
/// (omo#11437: one walk to the filesystem root per installed skill on every session open).
fn find_nearest_package(start_dir: &Path, memo: &mut HashMap<PathBuf, NearestPackage>) -> NearestPackage {
    let mut walked = Vec::new();
    let mut current_path = start_dir.to_path_buf();
    let nearest = loop {
        if let Some(known) = memo.get(&current_path) {
            break known.clone();
        }
        walked.push(current_path.clone());
        if current_path.join("package.json").exists() {
            break read_nearest_package(&current_path);
        }
        match current_path.parent() {
            Some(parent) if parent != current_path => current_path = parent.to_path_buf(),
            _ => break None,
        }
    };
    for path in walked {
        memo.insert(path, nearest.clone());
    }
    nearest
}

pub fn find_nearest_package_identity(resource_path: &str) -> Option<PackageIdentity> {
    identity_of(resource_path, &mut HashMap::new())
}

fn identity_of(resource_path: &str, memo: &mut HashMap<PathBuf, NearestPackage>) -> Option<PackageIdentity> {
    if resource_path.starts_with('<') {
        return None;
    }
    let normalized_resource_path = crate::paths::strip_verbatim_prefix(
        std::fs::canonicalize(resource_path).unwrap_or_else(|_| PathBuf::from(resource_path)),
    );
    let mut start_dir = PathBuf::from(resource_path);
    match std::fs::metadata(&start_dir) {
        Ok(metadata) if metadata.is_dir() => {}
        _ => {
            start_dir = start_dir.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("/"));
        }
    }
    let (root, name) = find_nearest_package(&start_dir, memo)?;
    let canonical_root =
        crate::paths::strip_verbatim_prefix(std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone()));
    let relative = normalized_resource_path
        .strip_prefix(&canonical_root)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| normalized_resource_path.to_string_lossy().into_owned());
    Some(PackageIdentity { key: format!("{name}:{relative}"), package_name: name })
}

/// First path per identity wins (CLI before settings before discovery); unpackaged paths pass through.
pub fn dedupe_paths_by_package_identity(paths: &[String]) -> Vec<String> {
    let mut deduped = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut memo = HashMap::new();
    for path in paths {
        if let Some(identity) = identity_of(path, &mut memo)
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
        assert!(identity.key.replace('\\', "/").ends_with("src/skills/a.md"));
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

    /// senpi test/suite/regressions/11437-skill-scan-per-open.test.ts: one walk per ancestor across many resources.
    #[test]
    fn the_memo_records_every_walked_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let pkg = tmp.path().join("pkg");
        std::fs::create_dir_all(pkg.join("a/b")).expect("mkdir");
        std::fs::write(pkg.join("package.json"), r#"{"name":"p"}"#).expect("write");
        let mut memo = HashMap::new();
        let nearest = find_nearest_package(&pkg.join("a/b"), &mut memo);
        assert_eq!(nearest.as_ref().map(|(_, name)| name.as_str()), Some("p"));
        assert!(memo.contains_key(&pkg.join("a/b")) && memo.contains_key(&pkg.join("a")));
        let paths = vec![
            pkg.join("a/b/x.md").to_string_lossy().into_owned(),
            pkg.join("a/b/x.md").to_string_lossy().into_owned(),
            pkg.join("a/y.md").to_string_lossy().into_owned(),
        ];
        assert_eq!(dedupe_paths_by_package_identity(&paths).len(), 2);
    }
}
