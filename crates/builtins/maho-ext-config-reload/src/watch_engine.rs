use std::{collections::{BTreeMap, BTreeSet}, path::{Path, PathBuf}, sync::Arc};
use sha2::{Digest, Sha256};
use super::watch_event_source::{subscribe, WatchErrorListener, WatchSubscription};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum WatchKind { Dir, DirRecursive }
pub type WatchFilter = Arc<dyn Fn(&Path) -> bool + Send + Sync>;
pub struct WatchTarget { pub id: String, pub kind: WatchKind, pub path: PathBuf, pub allow_list: Option<Vec<PathBuf>>, pub filter: Option<WatchFilter> }
#[derive(Default)]
pub struct RealChange { pub changed_paths: Vec<PathBuf>, pub created: Vec<PathBuf>, pub deleted: Vec<PathBuf> }
#[derive(Default)]
struct ScanResult { hashes: BTreeMap<PathBuf, String>, allowed_directories: BTreeSet<PathBuf>, scanned_directories: BTreeSet<PathBuf> }
pub struct ConfigReloadWatchEngine { targets: Vec<WatchTarget>, states: Vec<ScanResult>, closed: bool, on_error: Option<WatchErrorListener> }
pub struct NativeWatchEngine { pub engine: ConfigReloadWatchEngine, subscriptions: BTreeMap<PathBuf, WatchSubscription>, receiver: std::sync::mpsc::Receiver<Option<PathBuf>>, sender: std::sync::mpsc::Sender<Option<PathBuf>>, on_error: WatchErrorListener, debounce: std::time::Duration }
impl NativeWatchEngine {
    pub fn new(targets: Vec<WatchTarget>, on_error: WatchErrorListener) -> Result<Self, String> {
        Self::with_debounce(targets, on_error, std::time::Duration::from_millis(200))
    }
    pub fn with_debounce(targets: Vec<WatchTarget>, on_error: WatchErrorListener, debounce: std::time::Duration) -> Result<Self, String> {
        let engine = ConfigReloadWatchEngine::with_error_listener(targets, Arc::clone(&on_error)).map_err(|error| error.to_string())?;
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut state = Self { engine, subscriptions: BTreeMap::new(), receiver, sender, on_error, debounce };
        state.reconcile()?;
        Ok(state)
    }
    fn reconcile(&mut self) -> Result<(), String> {
        let wanted = self.engine.watched_directories();
        let removed: Vec<_> = self.subscriptions.keys().filter(|path| !wanted.contains(*path)).cloned().collect();
        for path in removed { if let Some(mut subscription) = self.subscriptions.remove(&path) { subscription.close()?; } }
        for path in wanted {
            if self.subscriptions.contains_key(&path) { continue; }
            let sender = self.sender.clone();
            let directory = path.clone();
            let targets: Vec<_> = self.engine.targets.iter().map(|target| (target.path.clone(), target.kind, target.allow_list.clone(), target.filter.clone())).collect();
            let subscription = subscribe(path.clone(), false, Arc::new(move |_, filename| {
                let mut affected = None;
                if let Some(filename) = filename {
                    if filename.is_absolute() || filename.components().any(|component| matches!(component, std::path::Component::ParentDir)) { return; }
                    let absolute = directory.join(filename);
                    if !targets.iter().any(|(root, kind, allowed, filter)| absolute.strip_prefix(root).is_ok_and(|relative| {
                        (*kind == WatchKind::DirRecursive || relative.components().count() <= 1)
                            && allowed.as_ref().is_none_or(|allowed| allowed.iter().any(|allowed| relative == allowed || relative.starts_with(allowed)))
                            && filter.as_ref().is_none_or(|filter| filter(relative))
                    })) { return; }
                    affected = Some(absolute);
                }
                let _ = sender.send(affected);
            }), Arc::clone(&self.on_error))?;
            subscription.ready()?;
            self.subscriptions.insert(path, subscription);
        }
        Ok(())
    }
    pub fn next_change(&mut self, timeout: std::time::Duration) -> Result<RealChange, String> {
        let first = self.receiver.recv_timeout(timeout).map_err(|error| error.to_string())?;
        let mut full = first.is_none();
        let mut affected: BTreeSet<_> = first.into_iter().collect();
        loop {
            match self.receiver.recv_timeout(self.debounce) {
                Ok(path) => { full |= path.is_none(); affected.extend(path); },
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                Err(error) => return Err(error.to_string()),
            }
        }
        let change = if full { self.engine.evaluate() } else { self.engine.evaluate_affected(&affected) }.map_err(|error| error.to_string())?;
        self.reconcile()?;
        Ok(change)
    }
    pub fn close(&mut self) -> Result<(), String> {
        self.engine.close();
        let mut errors = Vec::new();
        for (_, mut subscription) in std::mem::take(&mut self.subscriptions) { if let Err(error) = subscription.close() { errors.push(error); } }
        if errors.is_empty() { Ok(()) } else { Err(errors.join("; ")) }
    }
}
impl Drop for NativeWatchEngine { fn drop(&mut self) { if let Err(error) = self.close() { (self.on_error)(error, PathBuf::new()); } } }
impl ConfigReloadWatchEngine {
    pub fn new(targets: Vec<WatchTarget>) -> Result<Self, std::io::Error> {
        Self::create(targets, None)
    }
    pub fn with_error_listener(targets: Vec<WatchTarget>, on_error: WatchErrorListener) -> Result<Self, std::io::Error> {
        Self::create(targets, Some(on_error))
    }
    fn create(targets: Vec<WatchTarget>, on_error: Option<WatchErrorListener>) -> Result<Self, std::io::Error> {
        let states = targets.iter().map(|target| scan(target, on_error.as_ref())).collect::<Result<Vec<_>, _>>()?;
        Ok(Self { targets, states, closed: false, on_error })
    }
    pub fn close(&mut self) { self.closed = true; }
    pub fn get_baseline_snapshot(&self) -> BTreeMap<PathBuf, String> { self.states.iter().flat_map(|state| state.hashes.clone()).collect() }
    pub fn watched_directories(&self) -> BTreeSet<PathBuf> {
        self.targets.iter().zip(&self.states).flat_map(|(target, state)| std::iter::once(target.path.clone()).chain(state.scanned_directories.iter().cloned())).collect()
    }
    pub fn evaluate(&mut self) -> Result<RealChange, std::io::Error> {
        self.evaluate_paths(None)
    }
    pub fn evaluate_affected(&mut self, paths: &BTreeSet<PathBuf>) -> Result<RealChange, std::io::Error> {
        self.evaluate_paths(Some(paths))
    }
    fn evaluate_paths(&mut self, paths: Option<&BTreeSet<PathBuf>>) -> Result<RealChange, std::io::Error> {
        let mut changes = BTreeMap::new();
        if self.closed { return Ok(RealChange::default()); }
        for (target, state) in self.targets.iter().zip(&mut self.states) {
            let next = if let Some(paths) = paths {
                let prefixes: Vec<_> = paths.iter().filter_map(|path| path.strip_prefix(&target.path).ok().filter(|relative| matches(target, relative)).map(|relative| (path, relative))).collect();
                if prefixes.is_empty() { continue; }
                let mut next = ScanResult { hashes: state.hashes.clone(), allowed_directories: state.allowed_directories.clone(), scanned_directories: state.scanned_directories.clone() };
                for (path, relative) in prefixes {
                    next.hashes.retain(|key, _| !key.starts_with(path));
                    next.allowed_directories.retain(|key| !key.starts_with(path));
                    next.scanned_directories.retain(|key| !key.starts_with(path));
                    scan_path(target, path, relative, &mut next, self.on_error.as_ref())?;
                }
                next
            } else { scan(target, self.on_error.as_ref())? };
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
fn scan(target: &WatchTarget, on_error: Option<&WatchErrorListener>) -> Result<ScanResult, std::io::Error> {
    let mut result = ScanResult::default();
    scan_path(target, &target.path, Path::new(""), &mut result, on_error)?;
    Ok(result)
}
fn scan_path(target: &WatchTarget, absolute: &Path, relative: &Path, result: &mut ScanResult, on_error: Option<&WatchErrorListener>) -> Result<(), std::io::Error> {
    if let Err(error) = scan_entry(target, absolute, relative, result, on_error) {
        if let Some(listener) = on_error { listener(error.to_string(), absolute.into()); } else { return Err(error); }
    }
    Ok(())
}
fn scan_entry(target: &WatchTarget, absolute: &Path, relative: &Path, result: &mut ScanResult, on_error: Option<&WatchErrorListener>) -> Result<(), std::io::Error> {
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
        scan_path(target, &child.path(), &child_relative, result, on_error)?;
    }
    Ok(())
}
