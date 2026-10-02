use std::collections::HashMap;

#[derive(Default)]
pub struct InjectionCache { sessions: HashMap<String, Vec<String>> }
impl InjectionCache {
    pub fn has_injected(&self, session: &str, directory: &str) -> bool { self.sessions.get(session).is_some_and(|paths| paths.iter().any(|path| path == directory)) }
    pub fn mark_injected(&mut self, session: &str, directory: &str) {
        let paths = self.sessions.entry(session.to_owned()).or_default();
        if !paths.iter().any(|path| path == directory) { paths.push(directory.to_owned()); }
    }
    pub fn cache_size(&self, session: &str) -> usize { self.sessions.get(session).map_or(0, Vec::len) }
    pub fn list_injected(&self, session: &str) -> &[String] { self.sessions.get(session).map_or(&[], Vec::as_slice) }
    pub fn clear_session(&mut self, session: &str) { self.sessions.remove(session); }
    pub fn clear_all(&mut self) { self.sessions.clear(); }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn injected_when_marked() { let mut cache = InjectionCache::default(); cache.mark_injected("a", "/src"); assert!(cache.has_injected("a", "/src")); }
    #[test] fn absent_when_unmarked() { assert!(!InjectionCache::default().has_injected("a", "/src")); }
    #[test] fn deduplicated_when_remarked() { let mut cache = InjectionCache::default(); cache.mark_injected("a", "/src"); cache.mark_injected("a", "/src"); assert_eq!(cache.cache_size("a"), 1); }
    #[test] fn isolated_when_other_session_cleared() { let mut cache = InjectionCache::default(); cache.mark_injected("a", "/src"); cache.mark_injected("b", "/src"); cache.clear_session("a"); assert!(!cache.has_injected("a", "/src")); assert!(cache.has_injected("b", "/src")); }
    #[test] fn absent_when_session_cleared() { let mut cache = InjectionCache::default(); cache.mark_injected("a", "/src"); cache.clear_session("a"); assert!(!cache.has_injected("a", "/src")); }
    #[test] fn absent_when_all_cleared() { let mut cache = InjectionCache::default(); cache.mark_injected("a", "/src"); cache.mark_injected("b", "/lib"); cache.clear_all(); assert_eq!(cache.cache_size("a"), 0); assert_eq!(cache.cache_size("b"), 0); }
    #[test] fn counted_when_multiple_directories() { let mut cache = InjectionCache::default(); for path in ["/a", "/b", "/c"] { cache.mark_injected("a", path); } assert_eq!(cache.cache_size("a"), 3); }
    #[test] fn zero_when_unknown_session() { assert_eq!(InjectionCache::default().cache_size("unknown"), 0); }
    #[test] fn ordered_when_listed() { let mut cache = InjectionCache::default(); cache.mark_injected("a", "/a"); cache.mark_injected("a", "/b"); assert_eq!(cache.list_injected("a"), ["/a", "/b"]); }
}
