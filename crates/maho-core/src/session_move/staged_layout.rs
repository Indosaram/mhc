//! Port of senpi packages/coding-agent/src/core/session-move/staged-layout.ts.

use std::collections::HashMap;

use super::bundle::{BundleFile, MoveManifest, shown};

pub const STAGED_PAYLOADS: &str = ".payloads";

pub fn staged_name(manifest: &MoveManifest, item: &BundleFile) -> String {
    if item.path == manifest.session.path {
        manifest.session_file_name.clone()
    } else {
        item.path
            .strip_prefix("files/")
            .unwrap_or(&item.path)
            .to_owned()
    }
}

pub fn staged_names(manifest: &MoveManifest) -> Vec<String> {
    let mut names = Vec::with_capacity(
        1 + manifest.sidecars.len()
            + manifest
                .payloads
                .iter()
                .map(|p| p.files.len())
                .sum::<usize>(),
    );
    names.push(staged_name(manifest, &manifest.session));
    for item in &manifest.sidecars {
        names.push(staged_name(manifest, item));
    }
    for payload in &manifest.payloads {
        for file in &payload.files {
            names.push(format!("{STAGED_PAYLOADS}/{}", file.path));
        }
    }
    names
}

pub fn staged_twice(manifest: &MoveManifest) -> Option<String> {
    let mut staged: Vec<(String, &str)> = Vec::new();
    staged.push((
        staged_name(manifest, &manifest.session),
        &manifest.session.path,
    ));
    for item in &manifest.sidecars {
        staged.push((staged_name(manifest, item), &item.path));
    }
    for payload in &manifest.payloads {
        for file in &payload.files {
            staged.push((format!("{STAGED_PAYLOADS}/{}", file.path), &file.path));
        }
    }
    let mut from: HashMap<String, &str> = HashMap::new();
    for (name, path) in staged {
        if let Some(&other) = from.get(&name)
            && other != path
        {
            return Some(format!(
                "{} and {} would be staged as one file",
                shown(path),
                shown(other)
            ));
        }
        from.insert(name, path);
    }
    None
}
