//! Committed session-to-memory identity binding.
use memory_core::support::sha256::sha256_hex;

pub const MEMORY_BINDING_CUSTOM_TYPE: &str = "senpi-memory.session-binding";

#[derive(Clone, Debug, PartialEq)]
pub struct MemorySessionBinding {
    pub identity: String,
    pub repo_path_hash: String,
    pub bound_at: f64,
}

pub fn create_memory_binding(identity: &str, repo_path: &str, bound_at: f64) -> MemorySessionBinding {
    MemorySessionBinding { identity: identity.to_owned(), repo_path_hash: sha256_hex(repo_path.as_bytes()), bound_at }
}

pub struct SessionEntryLike {
    pub entry_type: String,
    pub custom_type: Option<String>,
    pub data: Option<MemorySessionBinding>,
}

pub fn find_latest_memory_binding(entries: &[SessionEntryLike]) -> Option<&MemorySessionBinding> {
    entries.iter().rev().find_map(|entry| {
        if entry.entry_type == "custom" && entry.custom_type.as_deref() == Some(MEMORY_BINDING_CUSTOM_TYPE) {
            entry.data.as_ref()
        } else { None }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stable_hash_hides_path() {
        let binding = create_memory_binding("agent-123", "/private/memory/repo", 42.0);
        assert_eq!(binding.repo_path_hash.len(), 64);
        assert!(binding.repo_path_hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(binding.repo_path_hash, create_memory_binding("agent-123", "/private/memory/repo", 42.0).repo_path_hash);
    }
    #[test]
    fn latest_valid_binding_wins() {
        let old = create_memory_binding("old", "a", 1.0);
        let new = create_memory_binding("new", "b", 2.0);
        let entries = [SessionEntryLike { entry_type: "custom".into(), custom_type: Some(MEMORY_BINDING_CUSTOM_TYPE.into()), data: Some(old) }, SessionEntryLike { entry_type: "custom".into(), custom_type: Some(MEMORY_BINDING_CUSTOM_TYPE.into()), data: None }, SessionEntryLike { entry_type: "custom".into(), custom_type: Some(MEMORY_BINDING_CUSTOM_TYPE.into()), data: Some(new.clone()) }];
        assert_eq!(find_latest_memory_binding(&entries), Some(&new));
    }
}
