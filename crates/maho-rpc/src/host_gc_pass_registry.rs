//! The opportunistic gc passes this process has in flight, keyed by agent directory.
//! `host_gc_pass.rs` starts them; this leaf only records them, so a caller that must not remove an
//! agent directory under a running pass (a test teardown, senpi#4382) can join them without
//! loading the gc module graph.

use futures::future::{BoxFuture, FutureExt, Shared};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

struct TrackedPass {
    id: u64,
    future: Shared<BoxFuture<'static, ()>>,
}

#[derive(Default)]
pub struct HostGcPassRegistry {
    passes: Mutex<HashMap<PathBuf, TrackedPass>>,
    next_id: AtomicU64,
}

impl HostGcPassRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn running(&self, agent_dir: &Path) -> bool {
        self.passes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(agent_dir)
    }

    pub fn track(
        &self,
        agent_dir: PathBuf,
        pass: BoxFuture<'static, ()>,
    ) -> Shared<BoxFuture<'static, ()>> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        let shared = pass.shared();
        self.passes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                agent_dir.clone(),
                TrackedPass {
                    id,
                    future: shared.clone(),
                },
            );
        shared
    }

    pub fn untrack_if_id(&self, agent_dir: &Path, id: u64) {
        let mut passes = self.passes.lock().unwrap_or_else(PoisonError::into_inner);
        if passes.get(agent_dir).is_some_and(|entry| entry.id == id) {
            passes.remove(agent_dir);
        }
    }

    pub async fn settled(&self, agent_dir: &Path) {
        let fut = self
            .passes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(agent_dir)
            .map(|entry| entry.future.clone());
        if let Some(fut) = fut {
            fut.await;
        }
    }

    pub fn within(&self, path: &Path) -> Vec<PathBuf> {
        self.passes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .keys()
            .filter(|agent_dir| lexical_contains(path, &agent_dir.join("rpc-host-daemon")))
            .cloned()
            .collect()
    }

    pub async fn settled_within(&self, path: &Path) {
        let futs: Vec<Shared<BoxFuture<'static, ()>>> = {
            let passes = self.passes.lock().unwrap_or_else(PoisonError::into_inner);
            passes
                .iter()
                .filter(|(agent_dir, _)| lexical_contains(path, &agent_dir.join("rpc-host-daemon")))
                .map(|(_, entry)| entry.future.clone())
                .collect()
        };
        futures::future::join_all(futs).await;
    }
}

fn global_registry() -> &'static HostGcPassRegistry {
    static REGISTRY: OnceLock<HostGcPassRegistry> = OnceLock::new();
    REGISTRY.get_or_init(HostGcPassRegistry::new)
}

pub fn opportunistic_host_gc_running(agent_dir: &Path) -> bool {
    global_registry().running(agent_dir)
}

pub fn track_opportunistic_host_gc(agent_dir: PathBuf, pass: BoxFuture<'static, ()>) {
    let registry = global_registry();
    let id = registry.next_id.fetch_add(1, Ordering::SeqCst) + 1;
    let shared = pass.shared();
    registry
        .passes
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(
            agent_dir.clone(),
            TrackedPass {
                id,
                future: shared.clone(),
            },
        );
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            shared.await;
            registry.untrack_if_id(&agent_dir, id);
        });
    }
}

/// Resolves once the pass this process scheduled for `agent_dir` (if any) has finished.
pub async fn settled_opportunistic_host_gc(agent_dir: &Path) {
    global_registry().settled(agent_dir).await;
}

/// The agent directories with a pass in flight whose `rpc-host-daemon` (where a pass writes) lies inside `path`.
pub fn opportunistic_host_gc_within(path: &Path) -> Vec<PathBuf> {
    global_registry().within(path)
}

pub async fn settled_opportunistic_host_gc_within(path: &Path) {
    global_registry().settled_within(path).await;
}

fn lexical_resolve(path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    };
    let mut resolved = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !resolved.pop() && !resolved.has_root() {
                    resolved.push("..");
                }
            }
            other => resolved.push(other.as_os_str()),
        }
    }
    resolved
}

// Lexical containment: a removal through another spelling of the same directory (such as macOS
// /var vs /private/var) is not joined.
fn lexical_contains(parent: &Path, child: &Path) -> bool {
    let parent = lexical_resolve(parent);
    let child = lexical_resolve(child);
    let p_comps: Vec<_> = parent.components().collect();
    let c_comps: Vec<_> = child.components().collect();
    c_comps.len() >= p_comps.len() && c_comps[..p_comps.len()] == p_comps[..]
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn tracks_settles_and_filters_within_lexically() {
        let reg = HostGcPassRegistry::new();
        let (tx, rx) = oneshot::channel::<()>();
        let dir = PathBuf::from("/tmp/gc-reg-test/agent-a");
        let shared = reg.track(
            dir.clone(),
            async move {
                let _ = rx.await;
            }
            .boxed(),
        );
        assert!(reg.running(&dir));
        assert_eq!(reg.within(Path::new("/tmp/gc-reg-test")), vec![dir.clone()]);
        assert!(reg.within(Path::new("/tmp/gc-reg-other")).is_empty());

        let wait_reg = shared.clone();
        let waiter = tokio::spawn(async move {
            wait_reg.await;
        });
        let _ = tx.send(());
        waiter.await.expect("join");
        reg.settled(&dir).await;
        reg.settled_within(Path::new("/tmp/gc-reg-test")).await;
    }

    #[test]
    fn lexical_contains_normalizes_dots_without_following_symlinks() {
        assert!(lexical_contains(
            Path::new("/tmp/root"),
            Path::new("/tmp/root/a/../b/rpc-host-daemon")
        ));
        assert!(!lexical_contains(
            Path::new("/tmp/root"),
            Path::new("/tmp/root/../other/rpc-host-daemon")
        ));
        assert!(!lexical_contains(
            Path::new("/tmp/root"),
            Path::new("/tmp/root-sibling/rpc-host-daemon")
        ));
    }
}
