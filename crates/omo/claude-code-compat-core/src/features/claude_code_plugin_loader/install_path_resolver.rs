use super::{
    plugin_key::derive_plugin_name_from_key,
    plugin_manifest::{find_plugin_manifest_path, read_manifest_from_path},
};
use crate::shared::logger::log;
use std::{
    cmp::Ordering,
    fs,
    path::{Path, PathBuf},
};

fn parse_semver_prefix(name: &str) -> Option<([u64; 3], bool)> {
    let mut parts = name.splitn(3, '.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let tail = parts.next()?;
    let end = tail
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(tail.len());
    let patch = tail.get(..end)?.parse().ok()?;
    let suffix = tail
        .get(end..)
        .is_some_and(|rest| rest.starts_with('-') || rest.starts_with('+'));
    Some(([major, minor, patch], suffix))
}
fn compare_candidate_priority(a: &str, b: &str) -> Ordering {
    (a == "unknown").cmp(&(b == "unknown")).then_with(|| {
        match (parse_semver_prefix(a), parse_semver_prefix(b)) {
            (Some((av, a_suffix)), Some((bv, b_suffix))) => bv
                .cmp(&av)
                .then_with(|| a_suffix.cmp(&b_suffix))
                .then_with(|| a.cmp(b)),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => a.cmp(b),
        }
    })
}
pub fn resolve_actual_install_path(
    configured_install_path: &Path,
    plugin_key: Option<&str>,
) -> Option<PathBuf> {
    if configured_install_path.exists() {
        return Some(configured_install_path.to_path_buf());
    }
    let parent = configured_install_path.parent()?;
    if !parent.exists() {
        return None;
    }
    let entries = match fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) => {
            log(
                &format!("Failed to scan plugin parent directory for fallback version: {error}"),
                None,
            );
            return None;
        }
    };
    let expected = plugin_key.map(derive_plugin_name_from_key);
    let mut candidates = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                log(
                    &format!(
                        "Failed to scan plugin parent directory for fallback version: {error}"
                    ),
                    None,
                );
                return None;
            }
        };
        let path = entry.path();
        let Some(manifest_path) = find_plugin_manifest_path(&path) else {
            continue;
        };
        if let Some(expected) = &expected {
            let Some(manifest) = read_manifest_from_path(&manifest_path) else {
                continue;
            };
            if manifest.name.is_empty() || &manifest.name != expected {
                continue;
            }
        }
        candidates.push((entry.file_name().to_string_lossy().into_owned(), path));
    }
    candidates.sort_by(|a, b| compare_candidate_priority(&a.0, &b.0));
    candidates.into_iter().next().map(|(_, path)| path)
}
