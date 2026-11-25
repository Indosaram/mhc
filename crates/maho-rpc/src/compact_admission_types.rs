//! senpi `modes/rpc/compact-admission-types.ts`: shared types for the compact admission
//! ledger, retention, settlement, and epoch state.

use crate::compact_admission_error::CompactAdmissionError;
use crate::compact_admission_producer::AdmissionProducerLifetime;
use crate::compact_admission_storage::AdmissionRow;
use futures::future::BoxFuture;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::watch;

pub type Answer = Map<String, Value>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionIdentity {
    pub socket_epoch: String,
    pub peer: String,
    pub admission_id: String,
    pub fingerprint: String,
    /// RULING-125: the request carried `retry: true` (a C4.4 resend); never part of the fingerprint.
    pub retry: bool,
}

pub type ReleaseFn = Box<dyn FnOnce() -> BoxFuture<'static, Result<(), String>> + Send>;
pub type ActivateFn = Box<dyn FnOnce() -> Result<(), String> + Send>;
pub type RefusalFn = Box<dyn Fn() -> Option<Answer> + Send>;

pub struct Produced {
    pub answer: Answer,
    pub release: Option<ReleaseFn>,
    pub activate: Option<ActivateFn>,
    pub refusal: Option<RefusalFn>,
}

impl Produced {
    pub fn simple(answer: Answer) -> Self {
        Self {
            answer,
            release: None,
            activate: None,
            refusal: None,
        }
    }
}

pub type HotSender = watch::Sender<Option<Result<Answer, CompactAdmissionError>>>;

pub struct Epoch {
    pub producer: Arc<AdmissionProducerLifetime>,
    pub peer: String,
    pub directory: PathBuf,
    pub hot: HashMap<String, HotSender>,
    pub failures: HashMap<String, AdmissionRow>,
    /// Terminal row keys to their admission IDs, least recently used first.
    pub done: Vec<(String, String)>,
    /// Digests of evicted IDs, oldest first.
    pub retired_order: VecDeque<String>,
    pub retired: HashSet<String>,
    pub serial_lock: Arc<tokio::sync::Mutex<()>>,
    pub ended: bool,
    pub waiting: usize,
}

impl Epoch {
    pub fn new(peer: String, directory: PathBuf) -> Self {
        Self {
            producer: Arc::new(AdmissionProducerLifetime::new()),
            peer,
            directory,
            hot: HashMap::new(),
            failures: HashMap::new(),
            done: Vec::new(),
            retired_order: VecDeque::new(),
            retired: HashSet::new(),
            serial_lock: Arc::new(tokio::sync::Mutex::new(())),
            ended: false,
            waiting: 0,
        }
    }
}
