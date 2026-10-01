//! Port of senpi packages/agent/src/harness/session/testing/storage-decorator.ts.

use std::collections::BTreeMap;

use maho_ai::types::BoxFuture;

use crate::harness::context::Context;
use crate::harness::session::session::{SessionError, SessionResult};
use crate::harness::session::types::{
    CommitResult, Entry, EntryScan, EntryStructure, SessionStats, Storage, StorageBranchScan, UsageRow, UsageScan,
    Write,
};
use crate::harness::session::values::{ListElement, ListReadOptions, StoredValue, Value, ValueList};

/// Test-only forwarding base for decorators that alter one part of Storage behavior.
pub struct StorageDecorator {
    delegate: std::sync::Arc<dyn Storage>,
}

impl StorageDecorator {
    pub fn new(delegate: std::sync::Arc<dyn Storage>) -> Self {
        Self { delegate }
    }

    pub fn delegate(&self) -> &std::sync::Arc<dyn Storage> {
        &self.delegate
    }
}

impl Storage for StorageDecorator {
    fn commit<'a>(&'a self, writes: Vec<Write>, context: &'a Context) -> BoxFuture<'a, SessionResult<CommitResult>> {
        self.delegate.commit(writes, context)
    }

    fn get_entries<'a>(
        &'a self,
        ids: Vec<String>,
        context: &'a Context,
    ) -> BoxFuture<'a, SessionResult<BTreeMap<String, Entry>>> {
        self.delegate.get_entries(ids, context)
    }

    fn get_value<'a>(
        &'a self,
        address: &'a Value,
        context: &'a Context,
    ) -> BoxFuture<'a, SessionResult<Option<StoredValue>>> {
        self.delegate.get_value(address, context)
    }

    fn scan_values<'a>(
        &'a self,
        prefix: &'a Value,
        context: &'a Context,
    ) -> BoxFuture<'a, SessionResult<Vec<StoredValue>>> {
        self.delegate.scan_values(prefix, context)
    }

    fn read_list<'a>(
        &'a self,
        address: &'a ValueList,
        options: Option<ListReadOptions>,
        context: &'a Context,
    ) -> BoxFuture<'a, SessionResult<Vec<ListElement>>> {
        self.delegate.read_list(address, options, context)
    }

    fn scan_branch<'a>(
        &'a self,
        query: StorageBranchScan,
        context: &'a Context,
    ) -> BoxFuture<'a, SessionResult<Vec<Entry>>> {
        self.delegate.scan_branch(query, context)
    }

    fn scan_branch_structure<'a>(
        &'a self,
        query: StorageBranchScan,
        context: &'a Context,
    ) -> BoxFuture<'a, SessionResult<Vec<EntryStructure>>> {
        self.delegate.scan_branch_structure(query, context)
    }

    fn scan_entries<'a>(&'a self, query: EntryScan, context: &'a Context) -> BoxFuture<'a, SessionResult<Vec<Entry>>> {
        self.delegate.scan_entries(query, context)
    }

    fn scan_usage<'a>(&'a self, query: UsageScan, context: &'a Context) -> BoxFuture<'a, SessionResult<Vec<UsageRow>>> {
        self.delegate.scan_usage(query, context)
    }

    fn get_stats<'a>(&'a self, context: &'a Context) -> BoxFuture<'a, SessionResult<SessionStats>> {
        self.delegate.get_stats(context)
    }

    fn close<'a>(&'a self, context: &'a Context) -> BoxFuture<'a, ()> {
        self.delegate.close(context)
    }
}

/// `CommitDiscarded`: every commit rejected after simulated storage loss.
pub fn commit_discarded(message: impl Into<String>) -> SessionError {
    SessionError::new(crate::harness::session::session::SessionErrorKind::CommitDiscarded, message)
}
