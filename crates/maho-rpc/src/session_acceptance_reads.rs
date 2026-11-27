//! senpi `modes/rpc/session-acceptance-reads.ts`: the read side of an acceptance lane,
//! refusing reads once a frozen root is released.

use crate::compact_current_components::CurrentComponentsView;
use crate::compact_lane_current::CompactLaneCurrent;
use crate::compact_read_lease::{CompactError, CompactReadReason};
use crate::compact_recovery_bytes::RecoveryBytesError;
use crate::compact_settled_messages::{CompactSettledMessages, SettledRead};
use crate::compact_state_view::CompactStateView;
use crate::session_acceptance_lane::FrozenAcceptedRoot;
use futures::future::BoxFuture;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Optional extension reads on a lane's current state (S03b-3b / S09 / S10b), bridged cleanly
/// until `CompactLaneCurrent` exposes all component/active/call cut readers directly.
pub trait CurrentCutReaders: Send + Sync {
    fn read_components(
        &self,
        cut_id: Option<u64>,
    ) -> BoxFuture<'_, Result<CurrentComponentsView, RecoveryBytesError>>;
    fn read_active(&self, cut_id: Option<u64>) -> BoxFuture<'_, Result<Value, RecoveryBytesError>>;
    fn read_calls(&self, cut_id: Option<u64>) -> BoxFuture<'_, Result<Value, RecoveryBytesError>>;
}

/// Live registration metadata for a published accepted root.
#[derive(Clone)]
pub struct PublishedRoot {
    pub id: u64,
    pub current_cut: Option<u64>,
    pub settled_owner: Arc<CompactSettledMessages>,
}

pub type BeforeAcceptFn = Arc<dyn Fn(&Value) + Send + Sync>;

/// The read side of an acceptance lane: every reader of a root goes through here and is refused
/// once the root is not live. Publishing and releasing roots stays with the lane.
pub struct AcceptedRootReads {
    /// S16 freezes any required repair cut before the dependent accepted mutation.
    pub before_accept: Option<BeforeAcceptFn>,
    live: HashSet<u64>,
    root_owners: HashMap<u64, PublishedRoot>,
    pub current: CompactLaneCurrent,
    extra_readers: Option<Arc<dyn CurrentCutReaders>>,
}

impl Default for AcceptedRootReads {
    fn default() -> Self {
        Self::new()
    }
}

impl AcceptedRootReads {
    pub fn new() -> Self {
        Self {
            before_accept: None,
            live: HashSet::new(),
            root_owners: HashMap::new(),
            current: CompactLaneCurrent::new(),
            extra_readers: None,
        }
    }

    pub fn with_extra_readers(mut self, readers: Arc<dyn CurrentCutReaders>) -> Self {
        self.extra_readers = Some(readers);
        self
    }

    pub fn live_roots(&self) -> usize {
        self.live.len()
    }

    /// True while `root_id` is readable: published by this lane and not released.
    pub fn holds(&self, root_id: u64) -> bool {
        self.live.contains(&root_id)
    }

    pub fn publish_root(&mut self, entry: PublishedRoot) {
        self.live.insert(entry.id);
        self.root_owners.insert(entry.id, entry);
    }

    pub fn release_root(&mut self, root_id: u64) -> Option<PublishedRoot> {
        self.live.remove(&root_id);
        let removed = self.root_owners.remove(&root_id);
        if let Some(ref entry) = removed {
            self.current.release(entry.current_cut);
        }
        removed
    }

    pub fn clear_roots(&mut self) {
        self.live.clear();
        let drained: Vec<_> = self.root_owners.drain().collect();
        for (_, entry) in drained {
            self.current.release(entry.current_cut);
        }
    }

    fn require_live(&self, root_id: u64) -> Result<&PublishedRoot, CompactError> {
        if !self.live.contains(&root_id) {
            return Err(CompactError::read(CompactReadReason::Released));
        }
        self.root_owners
            .get(&root_id)
            .ok_or_else(|| CompactError::read(CompactReadReason::Released))
    }

    pub async fn read_current(&self, root_id: u64) -> Result<CompactStateView, CompactError> {
        let entry = self.require_live(root_id)?;
        self.current.read(entry.current_cut).await
    }

    pub async fn read_current_components(
        &self,
        root_id: u64,
    ) -> Result<CurrentComponentsView, RecoveryBytesError> {
        let entry = self
            .require_live(root_id)
            .map_err(|_| RecoveryBytesError::Aborted)?;
        match &self.extra_readers {
            Some(readers) => readers.read_components(entry.current_cut).await,
            None => Err(RecoveryBytesError::Unavailable),
        }
    }

    pub async fn read_current_active(&self, root_id: u64) -> Result<Value, RecoveryBytesError> {
        let entry = self
            .require_live(root_id)
            .map_err(|_| RecoveryBytesError::Aborted)?;
        match &self.extra_readers {
            Some(readers) => readers.read_active(entry.current_cut).await,
            None => Err(RecoveryBytesError::Unavailable),
        }
    }

    pub async fn read_current_calls(&self, root_id: u64) -> Result<Value, RecoveryBytesError> {
        let entry = self
            .require_live(root_id)
            .map_err(|_| RecoveryBytesError::Aborted)?;
        match &self.extra_readers {
            Some(readers) => readers.read_calls(entry.current_cut).await,
            None => Err(RecoveryBytesError::Unavailable),
        }
    }

    pub async fn read_settled(
        &self,
        root_id: u64,
        root: &FrozenAcceptedRoot,
    ) -> Result<SettledRead, CompactError> {
        let entry = self.require_live(root_id)?;
        let Some(ref settled) = root.settled else {
            return Ok(SettledRead {
                settled_messages: Vec::new(),
                owners: Vec::new(),
            });
        };
        entry.settled_owner.read(settled).await
    }

    pub fn settled_owner(&self, root_id: u64) -> Result<&CompactSettledMessages, CompactError> {
        Ok(self.require_live(root_id)?.settled_owner.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_content_store::CompactContentStoreOptions;
    use crate::session_acceptance_lane::SessionAcceptanceLane;

    #[tokio::test]
    async fn released_root_refuses_reads() {
        let dir = tempfile::tempdir().expect("tempdir");
        let settled = Arc::new(CompactSettledMessages::new(
            "e1".into(),
            CompactContentStoreOptions {
                dir: dir.path().join("settled").display().to_string(),
                ..Default::default()
            },
            |_| {},
        ));
        let mut lane = SessionAcceptanceLane::default();
        lane.bootstrap(Default::default(), Some("e1".into()));
        let root = lane.freeze();

        let mut reads = AcceptedRootReads::new();
        reads.publish_root(PublishedRoot {
            id: 1,
            current_cut: None,
            settled_owner: settled,
        });
        assert!(reads.holds(1));
        assert_eq!(reads.live_roots(), 1);
        assert!(reads.read_settled(1, &root).await.is_ok());

        reads.release_root(1);
        assert!(!reads.holds(1));
        assert_eq!(reads.live_roots(), 0);
        assert!(reads.read_settled(1, &root).await.is_err());
        assert!(reads.read_current(1).await.is_err());
    }
}
