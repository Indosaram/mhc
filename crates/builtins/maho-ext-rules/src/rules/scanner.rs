use std::{collections::BTreeSet, fs, path::{Path, PathBuf}};
use super::constants::{RULE_FILE_EXTENSIONS, SCANNER_EXCLUDED_DIRS};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScannedFile { pub path: PathBuf, pub real_path: PathBuf }

pub fn scan_rule_files(root: &Path, excluded_dirs: Option<&[&str]>, max_depth: Option<usize>) -> Vec<ScannedFile> {
    let Ok(root) = std::path::absolute(root) else { return Vec::new(); };
    if !root.is_dir() { return Vec::new(); }
    let mut results = Vec::new();
    scan_directory(&root, 0, max_depth.unwrap_or(10), excluded_dirs.unwrap_or(SCANNER_EXCLUDED_DIRS), &mut BTreeSet::new(), &mut results);
    results
}

fn scan_directory(directory: &Path, depth: usize, max_depth: usize, excluded: &[&str], visited: &mut BTreeSet<PathBuf>, results: &mut Vec<ScannedFile>) {
    let Ok(real_directory) = fs::canonicalize(directory) else { return; };
    if !visited.insert(real_directory) { return; }
    let Ok(entries) = fs::read_dir(directory) else { return; };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    let collator = icu_collator::CollatorBorrowed::new_root(Default::default());
    entries.sort_by(|left, right| collator.compare(&left.file_name().to_string_lossy(), &right.file_name().to_string_lossy()));
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Ok(metadata) = fs::metadata(&path) else { continue; };
        if metadata.is_dir() {
            if depth < max_depth && !excluded.contains(&name.as_ref()) {
                scan_directory(&path, depth.saturating_add(1), max_depth, excluded, visited, results);
            }
        } else if metadata.is_file() && RULE_FILE_EXTENSIONS.iter().any(|extension| name.ends_with(extension)) {
            let real_path = fs::symlink_metadata(&path).ok().filter(|metadata| metadata.is_symlink())
                .and_then(|_| fs::canonicalize(&path).ok()).unwrap_or_else(|| path.clone());
            results.push(ScannedFile { path, real_path });
        }
    }
}
