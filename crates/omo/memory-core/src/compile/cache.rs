//! Revision-keyed in-memory cache for compiled prompt projections.

use super::block_persist::{CompiledBlockStore, StoreErrorHandler};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::git::GitMemoryRepo;
use crate::support::sha256::sha256_hex;

use super::compile::{
    CompileError, CompileMemoryBlockOptions, MemoryExpiry, compile_memory_block_with_reads, now_ms,
};
use super::expiry_slot::expiry_slot;
use super::stored_block::StoredBlock;

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

/// A compiled block and every `expires` instant its revision declares, passed or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledBlock {
    pub block: String,
    pub expiries: Vec<MemoryExpiry>,
}

/// One key's blocks at one revision, one per expiry slot. The first compile tells which instants
/// the revision declares; later compiles pick their block by slot.
#[derive(Debug, Clone)]
struct MemoryBlockCacheEntry {
    variant: String,
    expiries: Option<Vec<MemoryExpiry>>,
    blocks: BTreeMap<String, CompiledBlock>,
}

/// Bump with ANY change to the bytes a compile produces for the same inputs: persisted blocks are
/// keyed by it and by the caller's `build`, and a block compiled by older code must never be served.
pub const MEMORY_BLOCK_COMPILER_VERSION: u32 = 2;
pub type NowMs = Arc<dyn Fn() -> i64 + Send + Sync>;
pub type StoreLoader = Arc<dyn Fn() -> Result<(), String> + Send + Sync>;
pub type DegradedHandler = Arc<dyn Fn(&Path, &str, &[String]) + Send + Sync>;
#[derive(Default, Clone)]
pub struct MemoryBlockCacheOptions {
    pub store: Option<StoreLoader>,
    pub on_store_error: Option<StoreErrorHandler>,
    pub on_degraded: Option<DegradedHandler>,
    pub build: String,
    pub store_budget: Option<Duration>,
    /// The clock `expires` is evaluated against when a compile names no instant.
    pub now: Option<NowMs>,
}
struct StoreLoad {
    receiver: std::sync::mpsc::Receiver<Result<(), String>>,
}

/// Thread-safe in-memory cache for compiled prompt blocks, invalidated by HEAD revision.
#[derive(Default)]
pub struct MemoryBlockCache {
    options: MemoryBlockCacheOptions,
    stores: Mutex<BTreeMap<PathBuf, Arc<CompiledBlockStore>>>,
    module: Mutex<Option<StoreLoad>>,
    loaded: std::sync::atomic::AtomicBool,
    computing: Mutex<usize>,
    settled_signal: std::sync::Condvar,
    entries: Mutex<BTreeMap<String, MemoryBlockCacheEntry>>,
    compile_lock: Mutex<()>,
}

impl MemoryBlockCache {
    /// Creates an empty memory block cache.
    pub fn persistent(build: String) -> Self {
        Self::with_options(MemoryBlockCacheOptions {
            build,
            store: Some(Arc::new(|| Ok(()))),
            on_store_error: Some(Arc::new(|error| {
                eprintln!("memory block persistence: {error}")
            })),
            on_degraded: Some(Arc::new(|repo, revision, paths| {
                eprintln!(
                    "degraded memory projection {} {revision}: {paths:?}",
                    repo.display()
                )
            })),
            ..Default::default()
        })
    }
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_options(options: MemoryBlockCacheOptions) -> Self {
        Self {
            options,
            ..Self::default()
        }
    }

    fn store_within(&self, dir: &Path) -> Option<Arc<CompiledBlockStore>> {
        let mut stores = self.stores.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(store) = stores.get(dir) {
            return Some(store.clone());
        }
        let loader = self.options.store.as_ref()?;
        if !self.loaded.load(std::sync::atomic::Ordering::Acquire) {
            let mut module = self.module.lock().unwrap_or_else(|e| e.into_inner());
            if module.is_none() {
                let (tx, receiver) = std::sync::mpsc::channel();
                let loader = loader.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(loader());
                });
                *module = Some(StoreLoad { receiver });
            }
            match module.as_ref()?.receiver.recv_timeout(
                self.options
                    .store_budget
                    .unwrap_or(Duration::from_millis(25)),
            ) {
                Ok(Ok(())) => {
                    self.loaded
                        .store(true, std::sync::atomic::Ordering::Release);
                    *module = None;
                }
                Ok(Err(error)) => {
                    *module = None;
                    if let Some(report) = &self.options.on_store_error {
                        report(error);
                    }
                    return None;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => return None,
                Err(error) => {
                    *module = None;
                    if let Some(report) = &self.options.on_store_error {
                        report(error.to_string());
                    }
                    return None;
                }
            }
        }
        let handler = self
            .options
            .on_store_error
            .clone()
            .unwrap_or_else(|| Arc::new(|_| {}));
        let store = Arc::new(CompiledBlockStore::new(dir, handler));
        stores.insert(dir.to_path_buf(), store.clone());
        Some(store)
    }

    pub fn remember(
        &self,
        repo: &GitMemoryRepo,
        name: &str,
        revision: &str,
        compute: impl FnOnce() -> Result<(String, bool), CompileError>,
    ) -> Result<String, CompileError> {
        let key = self.persist_key(revision, name)?;
        let store = self.store_within(&repo.dir);
        if let Some(value) = store.as_ref().and_then(|store| store.get(&key)) {
            return Ok(value);
        }
        *self.computing.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        let result = compute();
        let result = result.map(|(value, persist)| {
            if persist && let Some(store) = store {
                store.put(key, value.clone());
            }
            value
        });
        *self.computing.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
        self.settled_signal.notify_all();
        result
    }

    fn persist_key(&self, revision: &str, name: &str) -> Result<String, CompileError> {
        serde_json::to_string(&(
            MEMORY_BLOCK_COMPILER_VERSION,
            &self.options.build,
            revision,
            name,
        ))
        .map_err(|e| CompileError::Git(e.to_string()))
    }

    fn clock(&self) -> i64 {
        self.options.now.as_ref().map_or_else(now_ms, |now| now())
    }

    /// The block of `revision` at `as_of_ms`, from the persisted store when an earlier process
    /// stored the block of the same expiry slot, compiled (and stored, unless degraded) otherwise.
    fn compile_stored(
        &self,
        repo: &GitMemoryRepo,
        name: &str,
        revision: &str,
        options: &CompileMemoryBlockOptions,
        as_of_ms: i64,
    ) -> Result<CompiledBlock, CompileError> {
        let store = self.store_within(&repo.dir);
        let key = self.persist_key(revision, name)?;
        if let Some(stored) = store.as_ref().and_then(|store| store.get_block(&key))
            && stored.slot == expiry_slot(&stored.expiries, as_of_ms)
        {
            return Ok(CompiledBlock {
                block: stored.block,
                expiries: stored.expiries,
            });
        }
        *self.computing.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        let result = compile_memory_block_with_reads(repo, Some(revision), options, as_of_ms);
        let result = result.map(|compiled| {
            if !compiled.degraded.is_empty() {
                if let Some(report) = &self.options.on_degraded {
                    report(&repo.dir, revision, &compiled.degraded);
                }
            } else if let Some(store) = &store {
                store.put_block(
                    key,
                    &StoredBlock {
                        block: compiled.block.clone(),
                        slot: expiry_slot(&compiled.expiries, as_of_ms),
                        expiries: compiled.expiries.clone(),
                    },
                );
            }
            CompiledBlock {
                block: compiled.block,
                expiries: compiled.expiries,
            }
        });
        *self.computing.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
        self.settled_signal.notify_all();
        result
    }

    pub fn settled(&self) {
        let mut computing = self.computing.lock().unwrap_or_else(|e| e.into_inner());
        while *computing > 0 {
            computing = self
                .settled_signal
                .wait(computing)
                .unwrap_or_else(|e| e.into_inner());
        }
        drop(computing);
        for store in self
            .stores
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
        {
            store.settled();
        }
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
        self.compile_at_head(repo, revision.as_deref(), template, options)
    }

    /// [`Self::compile`] with a HEAD revision the caller already resolved: same cache entry, no
    /// second `git rev-parse`.
    pub fn compile_at_head(
        &self,
        repo: &GitMemoryRepo,
        head: Option<&str>,
        template: &str,
        options: &CompileMemoryBlockOptions,
    ) -> Result<String, CompileError> {
        let key = format!("{}:{}", hash_memory_template(template), options.agent_id);
        Ok(self.compile_keyed(repo, key, head, options, None)?.block)
    }

    /// Compiles a memory block at an explicit (pinned) revision, returning the cached string while
    /// template and revision match. Kept apart from the HEAD entry so a lagging pin never evicts it.
    pub fn compile_at_revision(
        &self,
        repo: &GitMemoryRepo,
        revision: Option<&str>,
        template: &str,
        options: &CompileMemoryBlockOptions,
    ) -> Result<String, CompileError> {
        Ok(self
            .compile_with_expiries(repo, revision, template, options, None)?
            .block)
    }

    /// [`Self::compile_at_revision`] evaluated at the pin instant `as_of_ms` (the cache clock when
    /// `None`), plus the revision's `expires` instants so a pinned caller can announce what expired
    /// after its pin.
    pub fn compile_with_expiries(
        &self,
        repo: &GitMemoryRepo,
        revision: Option<&str>,
        template: &str,
        options: &CompileMemoryBlockOptions,
        as_of_ms: Option<i64>,
    ) -> Result<CompiledBlock, CompileError> {
        let key = format!(
            "{}:{}:pinned",
            hash_memory_template(template),
            options.agent_id
        );
        self.compile_keyed(repo, key, revision, options, as_of_ms)
    }

    fn compile_keyed(
        &self,
        repo: &GitMemoryRepo,
        key: String,
        revision: Option<&str>,
        options: &CompileMemoryBlockOptions,
        as_of_ms: Option<i64>,
    ) -> Result<CompiledBlock, CompileError> {
        let _compile = self
            .compile_lock
            .lock()
            .map_err(|_| CompileError::Git("compile mutex poisoned".into()))?;
        let at = as_of_ms.unwrap_or_else(|| self.clock());
        let key = format!(
            "{key}:{}:{}",
            repo.dir.display(),
            serde_json::to_string(options).map_err(|e| CompileError::Git(e.to_string()))?
        );
        let variant = revision.unwrap_or("no-head").to_string();

        {
            let guard = self
                .entries
                .lock()
                .map_err(|_| CompileError::Git("mutex poisoned".to_string()))?;
            if let Some(existing) = guard.get(&key)
                && existing.variant == variant
                && let Some(expiries) = &existing.expiries
                && let Some(hit) = existing.blocks.get(&expiry_slot(expiries, at))
            {
                return Ok(hit.clone());
            }
        }

        let compiled = match revision {
            Some(revision) => {
                self.compile_stored(repo, &format!("block:{key}"), revision, options, at)
            }
            None => compile_memory_block_with_reads(repo, None, options, at).map(|result| {
                CompiledBlock {
                    block: result.block,
                    expiries: result.expiries,
                }
            }),
        };
        match compiled {
            Ok(compiled) => {
                let mut guard = self
                    .entries
                    .lock()
                    .map_err(|_| CompileError::Git("mutex poisoned".to_string()))?;
                let entry = guard
                    .entry(key)
                    .and_modify(|entry| {
                        if entry.variant != variant {
                            *entry = MemoryBlockCacheEntry {
                                variant: variant.clone(),
                                expiries: None,
                                blocks: BTreeMap::new(),
                            };
                        }
                    })
                    .or_insert_with(|| MemoryBlockCacheEntry {
                        variant,
                        expiries: None,
                        blocks: BTreeMap::new(),
                    });
                entry
                    .expiries
                    .get_or_insert_with(|| compiled.expiries.clone());
                entry
                    .blocks
                    .insert(expiry_slot(&compiled.expiries, at), compiled.clone());
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
