use std::{collections::{BTreeMap, BTreeSet}, path::{Path, PathBuf}};
use super::{constants::*, scanner::{ScannedFile, scan_rule_files}, types::RuleCandidate};

#[derive(Default)]
pub struct RuleDiscoveryCache {
    pub scanned_rule_files: BTreeMap<PathBuf, Vec<ScannedFile>>,
    pub single_file_info: BTreeMap<PathBuf, Option<PathBuf>>,
}

pub struct FinderOptions<'a> {
    pub project_root: Option<&'a Path>,
    pub target_file: Option<&'a Path>,
    pub home_dir: &'a Path,
    pub disabled_sources: &'a BTreeSet<String>,
    pub skip_user_home: bool,
}

pub fn find_rule_candidates(options: FinderOptions<'_>, cache: &mut RuleDiscoveryCache) -> Vec<RuleCandidate> {
    let mut candidates = Vec::new();
    if let Some(root) = options.project_root {
        let root = absolute(root);
        let start = options.target_file.map(absolute).and_then(|path| path.parent().map(Path::to_path_buf));
        let mut directories = Vec::new();
        let mut current = start.filter(|path| path.strip_prefix(&root).is_ok_and(|relative| !relative.to_string_lossy().starts_with(".."))).unwrap_or_else(|| root.clone());
        loop {
            directories.push(current.clone());
            if current == root { break; }
            let Some(parent) = current.parent() else { break; };
            current = parent.to_path_buf();
        }
        for (distance, directory) in directories.iter().enumerate() {
            for (parent, subdir) in PROJECT_RULE_SUBDIRS {
                let source = format!("{parent}/{subdir}");
                if !options.disabled_sources.contains(&source) {
                    add_directory(&mut candidates, cache, &directory.join(parent).join(subdir), &root, &source, distance, false);
                }
            }
        }
        for (distance, directory) in directories.iter().enumerate() {
            for source in PROJECT_SINGLE_FILES {
                if !options.disabled_sources.contains(*source) {
                    add_file(&mut candidates, cache, &directory.join(source), &root, source, distance, false);
                }
            }
        }
    }
    if !options.skip_user_home {
        let home = absolute(options.home_dir);
        for subdir in USER_HOME_RULE_SUBDIRS {
            let source = format!("~/{subdir}");
            if !options.disabled_sources.contains(&source) {
                add_directory(&mut candidates, cache, &home.join(subdir), &home, &source, GLOBAL_DISTANCE, true);
            }
        }
        for file in USER_HOME_SINGLE_FILES {
            let source = format!("~/{file}");
            if !options.disabled_sources.contains(&source) {
                add_file(&mut candidates, cache, &home.join(file), &home, &source, GLOBAL_DISTANCE, true);
            }
        }
    }
    candidates
}

fn absolute(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::ParentDir => { normalized.pop(); },
            std::path::Component::CurDir => {},
            std::path::Component::Normal(_) | std::path::Component::RootDir | std::path::Component::Prefix(_) => normalized.push(component.as_os_str()),
        }
    }
    normalized
}
fn candidate(path: &Path, real_path: PathBuf, root: &Path, source: &str, distance: usize, global: bool, single: bool) -> RuleCandidate {
    RuleCandidate { path: path.to_string_lossy().into_owned(), real_path: real_path.to_string_lossy().into_owned(), source: source.into(), distance,
        is_global: global, is_single_file: single, relative_path: path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/") }
}
fn add_directory(output: &mut Vec<RuleCandidate>, cache: &mut RuleDiscoveryCache, directory: &Path, root: &Path, source: &str, distance: usize, global: bool) {
    let files = cache.scanned_rule_files.entry(directory.to_path_buf()).or_insert_with(|| scan_rule_files(directory, None, None));
    for file in files { output.push(candidate(&file.path, std::fs::canonicalize(&file.path).unwrap_or_else(|_| file.path.clone()), root, source, distance, global, false)); }
}
fn add_file(output: &mut Vec<RuleCandidate>, cache: &mut RuleDiscoveryCache, path: &Path, root: &Path, source: &str, distance: usize, global: bool) {
    let real = cache.single_file_info.entry(path.to_path_buf()).or_insert_with(|| path.is_file().then(|| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())));
    if let Some(real) = real { output.push(candidate(path, real.clone(), root, source, distance, global, true)); }
}
