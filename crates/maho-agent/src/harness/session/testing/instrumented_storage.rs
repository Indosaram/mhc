//! Port of senpi packages/agent/src/harness/session/testing/instrumented-storage.ts.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use maho_ai::types::BoxFuture;

use crate::harness::context::Context;
use crate::harness::session::session::{SessionError, SessionResult};
use crate::harness::session::types::{
    CommitResult, Entry, EntryScan, EntryStructure, SessionStats, Storage, StorageBranchScan, UsageRow, UsageScan,
    Write,
};
use crate::harness::session::values::{ListElement, ListReadOptions, StoredValue, Value, ValueList};

use super::storage_decorator::StorageDecorator;

/// Test-only transparent Storage decorator that records commit admission.
pub struct InstrumentedStorage {
    decorator: StorageDecorator,
    commit_attempts: Arc<Mutex<Vec<Vec<Write>>>>,
}

impl InstrumentedStorage {
    pub fn new(delegate: Arc<dyn Storage>) -> Self {
        Self {
            decorator: StorageDecorator::new(delegate),
            commit_attempts: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn get_commit_attempts(&self) -> Vec<Vec<Write>> {
        self.commit_attempts.lock().expect("commit attempts").clone()
    }

    pub fn clear_commit_attempts(&self) {
        self.commit_attempts.lock().expect("commit attempts").clear();
    }
}

impl Storage for InstrumentedStorage {
    fn commit<'a>(&'a self, writes: Vec<Write>, context: &'a Context) -> BoxFuture<'a, SessionResult<CommitResult>> {
        Box::pin(async move {
            self.commit_attempts
                .lock()
                .expect("commit attempts")
                .push(writes.clone());
            self.decorator.commit(writes, context).await
        })
    }

    fn get_entries<'a>(
        &'a self,
        ids: Vec<String>,
        context: &'a Context,
    ) -> BoxFuture<'a, SessionResult<BTreeMap<String, Entry>>> {
        self.decorator.get_entries(ids, context)
    }

    fn get_value<'a>(
        &'a self,
        address: &'a Value,
        context: &'a Context,
    ) -> BoxFuture<'a, SessionResult<Option<StoredValue>>> {
        self.decorator.get_value(address, context)
    }

    fn scan_values<'a>(&'a self, prefix: &'a Value, context: &'a Context) -> BoxFuture<'a, SessionResult<Vec<StoredValue>>> {
        self.decorator.scan_values(prefix, context)
    }

    fn read_list<'a>(
        &'a self,
        address: &'a ValueList,
        options: Option<ListReadOptions>,
        context: &'a Context,
    ) -> BoxFuture<'a, SessionResult<Vec<ListElement>>> {
        self.decorator.read_list(address, options, context)
    }

    fn scan_branch<'a>(
        &'a self,
        query: StorageBranchScan,
        context: &'a Context,
    ) -> BoxFuture<'a, SessionResult<Vec<Entry>>> {
        self.decorator.scan_branch(query, context)
    }

    fn scan_branch_structure<'a>(
        &'a self,
        query: StorageBranchScan,
        context: &'a Context,
    ) -> BoxFuture<'a, SessionResult<Vec<EntryStructure>>> {
        self.decorator.scan_branch_structure(query, context)
    }

    fn scan_entries<'a>(&'a self, query: EntryScan, context: &'a Context) -> BoxFuture<'a, SessionResult<Vec<Entry>>> {
        self.decorator.scan_entries(query, context)
    }

    fn scan_usage<'a>(&'a self, query: UsageScan, context: &'a Context) -> BoxFuture<'a, SessionResult<Vec<UsageRow>>> {
        self.decorator.scan_usage(query, context)
    }

    fn get_stats<'a>(&'a self, context: &'a Context) -> BoxFuture<'a, SessionResult<SessionStats>> {
        self.decorator.get_stats(context)
    }

    fn close<'a>(&'a self, context: &'a Context) -> BoxFuture<'a, ()> {
        self.decorator.close(context)
    }
}

pub type InstrumentedCommitAttempt = Vec<Write>;

pub use crate::harness::session::session::SessionErrorKind as InstrumentedErrorKind;

pub type InstrumentedResult<T> = Result<T, SessionError>;
