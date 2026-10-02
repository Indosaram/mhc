use std::path::{Path, PathBuf};
pub const FS_WATCH_RETRY_DELAY_MS: u64 = 5000;
pub fn canonical_watch_path(path: &Path) -> PathBuf {
    if cfg!(windows) { std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()) } else { path.to_path_buf() }
}
pub async fn probe_directory_openable(directory: &Path) -> std::io::Result<()> {
    let mut directory = tokio::fs::read_dir(directory).await?;
    directory.next_entry().await?;
    Ok(())
}
pub struct FsWatcher { inner: notify::RecommendedWatcher }
pub fn close_watcher(watcher: Option<FsWatcher>) { drop(watcher); }
pub fn watch_with_error_handler(
    path: &Path,
    listener: impl Fn(&str, Option<String>) + Send + 'static,
    on_error: impl Fn() + Send + Sync + 'static,
    recursive: bool,
) -> Option<FsWatcher> {
    use notify::Watcher;
    let on_error = std::sync::Arc::new(on_error);
    let error_handler = on_error.clone();
    let root = canonical_watch_path(path);
    let callback_root = root.clone();
    let mut watcher = match notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        match event {
            Err(_) => error_handler(),
            Ok(event) => {
                let kind = match event.kind {
                    notify::EventKind::Create(_) | notify::EventKind::Remove(_) | notify::EventKind::Modify(notify::event::ModifyKind::Name(_)) => "rename",
                    notify::EventKind::Modify(_) => "change",
                    _ => return,
                };
                if event.paths.is_empty() { listener(kind, None); }
                for path in event.paths {
                    let name = if callback_root.is_dir() { path.strip_prefix(&callback_root).unwrap_or(&path) } else { path.file_name().map(Path::new).unwrap_or(&path) };
                    listener(kind, Some(name.to_string_lossy().into_owned()));
                }
            }
        }
    }) {
        Ok(watcher) => watcher,
        Err(_) => { on_error(); return None; }
    };
    if watcher.watch(&root, if recursive { notify::RecursiveMode::Recursive } else { notify::RecursiveMode::NonRecursive }).is_err() { on_error(); return None; }
    Some(FsWatcher { inner: watcher })
}
impl FsWatcher {
    pub fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
        use notify::Watcher;
        self.inner.unwatch(&canonical_watch_path(path))
    }
}
