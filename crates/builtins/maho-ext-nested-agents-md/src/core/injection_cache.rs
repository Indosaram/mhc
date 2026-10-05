use std::{collections::BTreeMap, path::{Path, PathBuf}};
#[derive(Default)]
pub struct InjectionCache { sessions: BTreeMap<String, Vec<PathBuf>> }
impl InjectionCache {
 pub fn has_injected(&self, session_key: &str, directory: &Path) -> bool { self.sessions.get(session_key).is_some_and(|dirs| dirs.iter().any(|dir| dir == directory)) }
 pub fn mark_injected(&mut self, session_key: &str, directory: &Path) { if !self.has_injected(session_key, directory) { self.sessions.entry(session_key.into()).or_default().push(directory.to_path_buf()); } }
 pub fn get_cache_size(&self, session_key: &str) -> usize { self.sessions.get(session_key).map_or(0, Vec::len) }
 pub fn list_injected(&self, session_key: &str) -> &[PathBuf] { self.sessions.get(session_key).map_or(&[], Vec::as_slice) }
 pub fn clear_session(&mut self, session_key: &str) { self.sessions.remove(session_key); }
 pub fn clear_all(&mut self) { self.sessions.clear(); }
}
