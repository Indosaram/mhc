//! senpi `modes/rpc/compact-verified-components.ts`: verified byte sources of one host's
//! frozen roots (RULING-69 (2)). A component's open-time verification runs once per frozen root,
//! and entries are dropped with the lease of the one token that pins it.

use crate::compact_recovery_bytes::FrozenBytes;
use std::collections::HashMap;
use std::sync::Arc;

/// A second checkpoint token tried to pin a frozen root another live token still pins:
/// the one-token-per-root invariant is broken.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("frozen root already pinned by token {holder}; token {token_id} cannot pin it")]
pub struct FrozenRootPinnedError {
    pub token_id: String,
    pub holder: String,
}

impl FrozenRootPinnedError {
    pub fn new(token_id: impl Into<String>, holder: impl Into<String>) -> Self {
        Self {
            token_id: token_id.into(),
            holder: holder.into(),
        }
    }
}

struct PinnedRoot {
    token: String,
    components: HashMap<String, Arc<dyn FrozenBytes>>,
}

/// Verified byte sources keyed by frozen root identity (`usize`, e.g. `Arc::as_ptr` or cut id)
/// and page ref string.
#[derive(Default)]
pub struct VerifiedComponents {
    roots: HashMap<usize, PinnedRoot>,
    pinned: HashMap<String, usize>,
}

impl VerifiedComponents {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn size(&self) -> usize {
        self.roots.values().map(|r| r.components.len()).sum()
    }

    /// Registers `token_id` as the root's one holder; idempotent for that token, an error for another.
    pub fn pin(&mut self, token_id: &str, root_key: usize) -> Result<(), FrozenRootPinnedError> {
        if let Some(held) = self.roots.get(&root_key) {
            if held.token != token_id {
                return Err(FrozenRootPinnedError::new(token_id, &held.token));
            }
            return Ok(());
        }
        self.roots.insert(
            root_key,
            PinnedRoot {
                token: token_id.to_owned(),
                components: HashMap::new(),
            },
        );
        self.pinned.insert(token_id.to_owned(), root_key);
        Ok(())
    }

    pub fn get(&self, root_key: usize, page_ref: &str) -> Option<Arc<dyn FrozenBytes>> {
        self.roots
            .get(&root_key)
            .and_then(|root| root.components.get(page_ref).cloned())
    }

    /// Cached only while `token_id` still pins `root_key`; once dropped, a late open caches nothing.
    pub fn set(
        &mut self,
        token_id: &str,
        root_key: usize,
        page_ref: &str,
        bytes: Arc<dyn FrozenBytes>,
    ) -> Result<(), FrozenRootPinnedError> {
        let Some(held) = self.roots.get_mut(&root_key) else {
            return Ok(());
        };
        if held.token != token_id {
            return Err(FrozenRootPinnedError::new(token_id, &held.token));
        }
        held.components.insert(page_ref.to_owned(), bytes);
        Ok(())
    }

    pub fn drop_token(&mut self, token_id: &str) {
        let Some(root_key) = self.pinned.remove(token_id) else {
            return;
        };
        self.roots.remove(&root_key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_recovery_bytes::{CatalogDomain, FrozenLeafBytes, catalog_digest};

    fn dummy_bytes() -> Arc<dyn FrozenBytes> {
        let digest = catalog_digest(CatalogDomain::Leaf, &[]);
        let read = Arc::new(|_| Box::pin(async { Ok(String::new()) }) as _);
        Arc::new(FrozenLeafBytes::new(digest, Vec::new(), vec![0], read))
    }

    #[test]
    fn pins_caches_and_drops_per_token() {
        let mut cache = VerifiedComponents::new();
        cache.pin("tok-1", 100).expect("pin");
        cache.pin("tok-1", 100).expect("idempotent");
        assert_eq!(
            cache.pin("tok-2", 100).expect_err("foreign pin").holder,
            "tok-1"
        );

        let bytes = dummy_bytes();
        cache
            .set("tok-1", 100, "c-0000000000000001:000", bytes)
            .expect("cache");
        assert_eq!(cache.size(), 1);
        assert!(cache.get(100, "c-0000000000000001:000").is_some());

        cache.drop_token("tok-1");
        assert_eq!(cache.size(), 0);
        assert!(cache.get(100, "c-0000000000000001:000").is_none());

        // Late set after drop caches nothing.
        cache
            .set("tok-1", 100, "c-0000000000000001:000", dummy_bytes())
            .expect("late noop");
        assert_eq!(cache.size(), 0);
    }
}
