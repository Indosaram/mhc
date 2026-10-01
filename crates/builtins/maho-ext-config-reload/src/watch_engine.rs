use std::{collections::{BTreeMap, BTreeSet}, path::{Path, PathBuf}, sync::Arc};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum WatchKind { Dir, DirRecursive }
pub type WatchFilter = Arc<dyn Fn(&Path) -> bool + Send + Sync>;
pub struct WatchTarget { pub id: String, pub kind: WatchKind, pub path: PathBuf, pub allow_list: Option<Vec<PathBuf>>, pub filter: Option<WatchFilter> }
#[derive(Default)]
pub struct RealChange { pub changed_paths: Vec<PathBuf>, pub created: Vec<PathBuf>, pub deleted: Vec<PathBuf> }
#[derive(Default)]
struct ScanResult { hashes: BTreeMap<PathBuf, String>, allowed_directories: BTreeSet<PathBuf>, scanned_directories: BTreeSet<PathBuf> }
pub struct ConfigReloadWatchEngine { targets: Vec<WatchTarget>, states: Vec<ScanResult>, closed: bool }
impl ConfigReloadWatchEngine {
    pub fn new(targets: Vec<WatchTarget>) -> Result<Self, std::io::Error> {
        let states = targets.iter().map(scan).collect::<Result<Vec<_>, _>>()?;
        Ok(Self { targets, states, closed: false })
    }
    pub fn close(&mut self) { self.closed = true; }
    pub fn get_baseline_snapshot(&self) -> BTreeMap<PathBuf, String> { self.states.iter().flat_map(|state| state.hashes.clone()).collect() }
    pub fn watched_directories(&self) -> BTreeSet<PathBuf> {
        self.targets.iter().zip(&self.states).flat_map(|(target, state)| std::iter::once(target.path.clone()).chain(state.scanned_directories.iter().cloned())).collect()
    }
    pub fn evaluate(&mut self) -> Result<RealChange, std::io::Error> {
        let mut changes = BTreeMap::new();
        if self.closed { return Ok(RealChange::default()); }
        for (target, state) in self.targets.iter().zip(&mut self.states) {
            let next = scan(target)?;
            for (path, hash) in &next.hashes { if state.hashes.get(path) != Some(hash) { changes.insert(path.clone(), (!state.hashes.contains_key(path), false)); } }
            for path in state.hashes.keys() { if !next.hashes.contains_key(path) { changes.insert(path.clone(), (false, true)); } }
            for path in &next.allowed_directories { if !state.allowed_directories.contains(path) { changes.insert(path.clone(), (true, false)); } }
            for path in &state.allowed_directories { if !next.allowed_directories.contains(path) { changes.insert(path.clone(), (false, true)); } }
            *state = next;
        }
        let changed_paths = changes.keys().cloned().collect();
        let created = changes.iter().filter(|(_, flags)| flags.0).map(|(path, _)| path.clone()).collect();
        let deleted = changes.iter().filter(|(_, flags)| flags.1).map(|(path, _)| path.clone()).collect();
        Ok(RealChange { changed_paths, created, deleted })
    }
}
fn matches(target: &WatchTarget, relative: &Path) -> bool {
    (target.kind == WatchKind::DirRecursive || relative.components().count() <= 1)
        && target.allow_list.as_ref().is_none_or(|allowed| allowed.iter().any(|allowed| relative == allowed || relative.starts_with(allowed)))
        && target.filter.as_ref().is_none_or(|filter| filter(relative))
}
fn explicitly_allowed(target: &WatchTarget, relative: &Path) -> bool { target.allow_list.as_ref().is_some_and(|allowed| allowed.iter().any(|allowed| relative == allowed || relative.starts_with(allowed))) }
fn scan(target: &WatchTarget) -> Result<ScanResult, std::io::Error> {
    let mut result = ScanResult::default();
    scan_path(target, &target.path, Path::new(""), &mut result)?;
    Ok(result)
}
fn scan_path(target: &WatchTarget, absolute: &Path, relative: &Path, result: &mut ScanResult) -> Result<(), std::io::Error> {
    if !relative.as_os_str().is_empty() && !matches(target, relative) { return Ok(()); }
    let entry = match std::fs::symlink_metadata(absolute) { Ok(entry) => entry, Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()), Err(error) => return Err(error) };
    if entry.is_symlink() { return Ok(()); }
    if entry.is_file() { result.hashes.insert(absolute.into(), format!("{:x}", Sha256::digest(std::fs::read(absolute)?))); return Ok(()); }
    if !entry.is_dir() { return Ok(()); }
    let non_root = !relative.as_os_str().is_empty();
    let allowed = non_root && explicitly_allowed(target, relative);
    if allowed { result.allowed_directories.insert(absolute.into()); }
    if non_root && (target.kind == WatchKind::Dir || relative.file_name().is_some_and(|name| name.to_string_lossy().starts_with('.')) && !allowed) { return Ok(()); }
    result.scanned_directories.insert(absolute.into());
    for child in std::fs::read_dir(absolute)? {
        let child = child?;
        if child.file_type()?.is_symlink() || child.file_name() == "node_modules" || child.file_name() == ".git" { continue; }
        let child_relative = relative.join(child.file_name());
        if child.file_type()?.is_dir() && child.file_name().to_string_lossy().starts_with('.') && !explicitly_allowed(target, &child_relative) { continue; }
        scan_path(target, &child.path(), &child_relative, result)?;
    }
    Ok(())
}
