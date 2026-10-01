//! Port of senpi packages/agent/src/harness/session/testing/gating-storage.ts.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use maho_ai::types::BoxFuture;
use tokio::sync::oneshot;

use crate::harness::context::Context;
use crate::harness::session::session::{SessionError, SessionResult};
use crate::harness::session::types::{CommitResult, Storage, Write};

use super::storage_decorator::{StorageDecorator, commit_discarded};

struct ParkedCommit {
    release: oneshot::Sender<()>,
    landing: oneshot::Receiver<()>,
}

struct PendingWaiter {
    count: usize,
    resolve: oneshot::Sender<()>,
}

#[derive(Default)]
struct GatingState {
    armed: bool,
    discarded: bool,
    queue: VecDeque<ParkedCommit>,
    waiters: Vec<PendingWaiter>,
}

/// Test-only storage decorator that deterministically parks admitted commits.
pub struct GatingStorage {
    decorator: StorageDecorator,
    state: Arc<Mutex<GatingState>>,
}

impl GatingStorage {
    pub fn new(delegate: Arc<dyn Storage>) -> Self {
        Self {
            decorator: StorageDecorator::new(delegate),
            state: Arc::new(Mutex::new(GatingState::default())),
        }
    }

    /// Fixture setup bypasses gating until explicitly armed.
    pub fn arm(&self) {
        self.state.lock().expect("gating state").armed = true;
    }

    pub fn pending(&self) -> usize {
        self.state.lock().expect("gating state").queue.len()
    }

    /// Wait until at least `count` commits are parked.
    pub async fn wait_pending(&self, count: usize) -> Result<(), SessionError> {
        if count == 0 {
            return Err(SessionError::new(
                crate::harness::session::session::SessionErrorKind::Invariant,
                "Pending commit count must be a positive safe integer",
            ));
        }
        let receiver = {
            let mut state = self.state.lock().expect("gating state");
            if state.discarded {
                return Err(commit_discarded("storage discarded"));
            }
            if state.queue.len() >= count {
                return Ok(());
            }
            let (sender, receiver) = oneshot::channel();
            state.waiters.push(PendingWaiter { count, resolve: sender });
            receiver
        };
        receiver
            .await
            .map_err(|_| commit_discarded("storage discarded"))
    }

    /// Release `count` commits in FIFO order and wait until each write lands.
    pub async fn next(&self, count: usize) -> Result<(), SessionError> {
        if count == 0 {
            return Err(SessionError::new(
                crate::harness::session::session::SessionErrorKind::Invariant,
                "Released commit count must be a positive safe integer",
            ));
        }
        for _ in 0..count {
            self.wait_pending(1).await?;
            let parked = {
                let mut state = self.state.lock().expect("gating state");
                state.queue.pop_front()
            };
            let parked = parked.ok_or_else(|| commit_discarded("No parked commit"))?;
            let _ = parked.release.send(());
            let _ = parked.landing.await;
        }
        Ok(())
    }

    /// Drop parked commits and permanently reject every later commit.
    pub fn discard(&self) {
        let mut state = self.state.lock().expect("gating state");
        if state.discarded {
            return;
        }
        state.discarded = true;
        state.queue.clear();
        state.waiters.clear();
    }

    fn notify_waiters(state: &mut GatingState) {
        let mut remaining: Vec<PendingWaiter> = Vec::new();
        for waiter in state.waiters.drain(..) {
            if state.queue.len() >= waiter.count {
                let _ = waiter.resolve.send(());
            } else {
                remaining.push(waiter);
            }
        }
        state.waiters = remaining;
    }
}

impl Storage for GatingStorage {
    fn commit<'a>(&'a self, writes: Vec<Write>, context: &'a Context) -> BoxFuture<'a, SessionResult<CommitResult>> {
        Box::pin(async move {
            let armed = {
                let state = self.state.lock().expect("gating state");
                if state.discarded {
                    return Err(commit_discarded("commit rejected: storage discarded"));
                }
                state.armed
            };
            if !armed {
                return self.decorator.commit(writes, context).await;
            }

            let (release_sender, release_receiver) = oneshot::channel();
            let (landed_sender, landed_receiver) = oneshot::channel();
            {
                let mut state = self.state.lock().expect("gating state");
                state.queue.push_back(ParkedCommit {
                    release: release_sender,
                    landing: landed_receiver,
                });
                Self::notify_waiters(&mut state);
            }

            let released = release_receiver.await;
            if released.is_err() {
                return Err(commit_discarded("commit discarded"));
            }
            let discarded = {
                let state = self.state.lock().expect("gating state");
                state.discarded
            };
            if discarded {
                return Err(commit_discarded("commit rejected: storage discarded"));
            }
            let result = self.decorator.commit(writes, context).await;
            let _ = landed_sender.send(());
            result
        })
    }

    fn get_entries<'a>(
        &'a self,
        ids: Vec<String>,
        context: &'a Context,
    ) -> BoxFuture<'a, SessionResult<std::collections::BTreeMap<String, crate::harness::session::types::Entry>>> {
        self.decorator.get_entries(ids, context)
    }

    fn get_value<'a>(
        &'a self,
        address: &'a crate::harness::session::values::Value,
        context: &'a Context,
    ) -> BoxFuture<'a, SessionResult<Option<crate::harness::session::values::StoredValue>>> {
        self.decorator.get_value(address, context)
    }

    fn scan_values<'a>(
        &'a self,
        prefix: &'a crate::harness::session::values::Value,
        context: &'a Context,
    ) -> BoxFuture<'a, SessionResult<Vec<crate::harness::session::values::StoredValue>>> {
        self.decorator.scan_values(prefix, context)
    }

    fn read_list<'a>(
        &'a self,
        address: &'a crate::harness::session::values::ValueList,
        options: Option<crate::harness::session::values::ListReadOptions>,
        context: &'a Context,
    ) -> BoxFuture<'a, SessionResult<Vec<crate::harness::session::values::ListElement>>> {
        self.decorator.read_list(address, options, context)
    }

    fn scan_branch<'a>(
        &'a self,
        query: crate::harness::session::types::StorageBranchScan,
        context: &'a Context,
    ) -> BoxFuture<'a, SessionResult<Vec<crate::harness::session::types::Entry>>> {
        self.decorator.scan_branch(query, context)
    }

    fn scan_branch_structure<'a>(
        &'a self,
        query: crate::harness::session::types::StorageBranchScan,
        context: &'a Context,
    ) -> BoxFuture<'a, SessionResult<Vec<crate::harness::session::types::EntryStructure>>> {
        self.decorator.scan_branch_structure(query, context)
    }

    fn scan_entries<'a>(
        &'a self,
        query: crate::harness::session::types::EntryScan,
        context: &'a Context,
    ) -> BoxFuture<'a, SessionResult<Vec<crate::harness::session::types::Entry>>> {
        self.decorator.scan_entries(query, context)
    }

    fn scan_usage<'a>(
        &'a self,
        query: crate::harness::session::types::UsageScan,
        context: &'a Context,
    ) -> BoxFuture<'a, SessionResult<Vec<crate::harness::session::types::UsageRow>>> {
        self.decorator.scan_usage(query, context)
    }

    fn get_stats<'a>(
        &'a self,
        context: &'a Context,
    ) -> BoxFuture<'a, SessionResult<crate::harness::session::types::SessionStats>> {
        self.decorator.get_stats(context)
    }

    fn close<'a>(&'a self, context: &'a Context) -> BoxFuture<'a, ()> {
        self.decorator.close(context)
    }
}
