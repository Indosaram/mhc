//! Revision-keyed in-memory cache for compiled prompt projections.

use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::git::GitMemoryRepo;
use crate::support::sha256::sha256_hex;

use super::compile::{CompileError, CompileMemoryBlockOptions, compile_memory_block_at_revision};

/// Structure version string for memory prompt templates.
pub const MEMORY_TEMPLATE_STRUCTURE_VERSION: &str = "senpi-memory-v2";

/// Computes a stable hex digest for a template string across cache lookups.
pub fn hash_memory_template(template: &str) -> String {
    let mut bytes =
        Vec::with_capacity(MEMORY_TEMPLATE_STRUCTURE_VERSION.len() + 1 + template.len());
    bytes.extend_from_slice(MEMORY_TEMPLATE_STRUCTURE_VERSION.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(template.as_bytes());
    sha256_hex(&bytes)
}

#[derive(Debug, Clone)]
struct MemoryBlockCacheEntry {
    variant: String,
    compiled: String,
}

/// Thread-safe in-memory cache for compiled prompt blocks, invalidated by HEAD revision.
#[derive(Debug, Default)]
pub struct MemoryBlockCache {
    entries: Mutex<BTreeMap<String, MemoryBlockCacheEntry>>,
}

impl MemoryBlockCache {
    /// Creates an empty memory block cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of distinct template and agent entries stored in the cache.
    pub fn size(&self) -> usize {
        self.entries
            .lock()
            .map(|guard| guard.len())
            .unwrap_or_default()
    }

    /// Compiles a memory block, returning the cached string if template and HEAD match.
    pub fn compile(
        &self,
        repo: &GitMemoryRepo,
        template: &str,
        options: &CompileMemoryBlockOptions,
    ) -> Result<String, CompileError> {
        let revision = repo.head().map_err(|e| CompileError::Git(e.to_string()))?;
        let key = format!("{}:{}", hash_memory_template(template), options.agent_id);
        let variant = revision.as_deref().unwrap_or("no-head").to_string();

        {
            let guard = self
                .entries
                .lock()
                .map_err(|_| CompileError::Git("mutex poisoned".to_string()))?;
            if let Some(existing) = guard.get(&key)
                && existing.variant == variant
            {
                return Ok(existing.compiled.clone());
            }
        }

        match compile_memory_block_at_revision(repo, revision.as_deref(), options) {
            Ok(compiled) => {
                let mut guard = self
                    .entries
                    .lock()
                    .map_err(|_| CompileError::Git("mutex poisoned".to_string()))?;
                guard.insert(
                    key,
                    MemoryBlockCacheEntry {
                        variant,
                        compiled: compiled.clone(),
                    },
                );
                Ok(compiled)
            }
            Err(err) => {
                if let Ok(mut guard) = self.entries.lock() {
                    guard.remove(&key);
                }
                Err(err)
            }
        }
    }

    /// Clears all cached entries.
    pub fn clear(&self) {
        if let Ok(mut guard) = self.entries.lock() {
            guard.clear();
        }
    }
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;
