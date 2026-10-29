use crate::lsp::effective_extension::effective_extension;
use crate::lsp::language_mappings::is_known_extension;
use indexmap::IndexMap;
use std::path::Path;

const SKIP_DIRECTORIES: [&str; 6] = ["node_modules", ".git", "dist", "build", ".next", "out"];
const MAX_SCAN_ENTRIES: usize = 500;

/// TS `inferExtensionFromDirectory`: most frequent known extension (first seen wins ties).
pub fn infer_extension_from_directory(directory: &Path) -> Option<String> {
    let mut counts: IndexMap<String, usize> = IndexMap::new();
    let mut scanned = 0usize;
    walk(directory, &mut counts, &mut scanned);
    let mut best: Option<(&String, usize)> = None;
    for (ext, count) in &counts {
        if best.is_none_or(|(_, max)| *count > max) {
            best = Some((ext, *count));
        }
    }
    best.map(|(ext, _)| ext.clone())
        .filter(|ext| !ext.is_empty())
}

fn walk(dir: &Path, counts: &mut IndexMap<String, usize>, scanned: &mut usize) {
    if *scanned >= MAX_SCAN_ENTRIES {
        return;
    }
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    // Node's readdirSync returns entries sorted by name (libuv scandir + alphasort).
    let mut entries: Vec<String> = read
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    entries.sort();
    for entry in entries {
        if *scanned >= MAX_SCAN_ENTRIES {
            return;
        }
        let full = dir.join(&entry);
        let Ok(meta) = std::fs::symlink_metadata(&full) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        *scanned += 1;
        if meta.is_dir() {
            if !SKIP_DIRECTORIES.contains(&entry.as_str()) {
                walk(&full, counts, scanned);
            }
        } else if meta.is_file() {
            let ext = effective_extension(&full.to_string_lossy());
            if !ext.is_empty() && is_known_extension(&ext) {
                *counts.entry(ext).or_insert(0) += 1;
            }
        }
    }
}
