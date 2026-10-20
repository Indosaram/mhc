use std::{collections::HashMap, io, path::{Component, Path, PathBuf}, sync::{LazyLock, Mutex}};
use futures::future::{BoxFuture, Shared};
use super::types::CommandRecord;

pub type CachedCommands = Shared<BoxFuture<'static, CommandRecord>>;

static COMMAND_LOADER_CACHE: LazyLock<Mutex<HashMap<PathBuf, CachedCommands>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn get_command_loader_cache_key(directory: Option<&Path>) -> io::Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    let resolved = cwd.join(directory.unwrap_or(&cwd));
    let mut normalized = PathBuf::new();
    for component in resolved.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => { normalized.pop(); }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => normalized.push(component.as_os_str()),
        }
    }
    Ok(std::fs::canonicalize(&normalized).unwrap_or(normalized))
}

pub fn get_cached_commands(key: &Path) -> Option<CachedCommands> {
    COMMAND_LOADER_CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(key).cloned()
}

pub fn set_cached_commands(key: PathBuf, commands: CachedCommands) {
    COMMAND_LOADER_CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(key, commands);
}

pub fn delete_cached_commands(key: &Path) {
    COMMAND_LOADER_CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(key);
}

pub fn clear_command_loader_cache() {
    COMMAND_LOADER_CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
}
