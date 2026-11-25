//! Port of senpi `packages/coding-agent/src/modes/rpc/session-event-writer-state.ts`.
//!
//! Shared state fields and locality/connection abstractions for the `SessionEventWriter` layers.

use crate::delegation_record::DelegationDescriptor;
use crate::remote_origin::RpcRemoteOrigin;
use crate::session_open_turns::SessionOpenTurns;
use crate::session_record_accounting::SessionRecordAccounting;
use crate::session_surface_dedupe::SurfaceDigestDedupe;
use std::collections::{BTreeMap, BTreeSet};

pub const DEFAULT_SHARED_STDIO_QUEUE_BYTES: usize = 64 * 1024 * 1024;
pub const DEFAULT_SHARED_STDIO_QUEUE_RECORDS: usize = 4096;

/// Shared locality metadata used by security and secret presentations.
#[derive(Debug, Clone, Default)]
pub struct WriterLocality {
    pub remote_connections: BTreeSet<String>,
    pub origins: BTreeMap<String, RpcRemoteOrigin>,
    pub delegations: BTreeMap<String, DelegationDescriptor>,
}

impl WriterLocality {
    pub fn is_remote(&self, connection: &str) -> bool {
        self.delegations.contains_key(connection) || self.origins.contains_key(connection)
    }

    pub fn origin_of(&self, connection: &str) -> Option<&RpcRemoteOrigin> {
        self.origins.get(connection)
    }

    pub fn delegation_of(&self, connection: &str) -> Option<&DelegationDescriptor> {
        self.delegations.get(connection)
    }
}

/// Core accounting state held by the event writer across sessions and connections.
#[derive(Default)]
pub struct SessionEventWriterState {
    pub records: SessionRecordAccounting,
    pub sealed_sessions: BTreeSet<String>,
    pub worker_sessions: BTreeSet<String>,
    pub open_turns: SessionOpenTurns,
    pub surface_dedupe: SurfaceDigestDedupe,
    pub locality: WriterLocality,
    pub reserved_close_records: usize,
    pub reserved_close_bytes: usize,
}
