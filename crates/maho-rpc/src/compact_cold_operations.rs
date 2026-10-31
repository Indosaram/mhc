//! senpi `modes/rpc/compact-cold-operations.ts`: the non-acceptance FIFO of settled-message status and
//! removal. Capacity rejection leaves work unqueued; callers await admitted operations.
use crate::compact_content_index::CompactContentIndex;
use crate::compact_read_lease::{CompactError, lock};
use crate::compact_transition::{CompactPersistence, CompactTransition};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// `message_status` or `message_remove`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactColdOperation {
    MessageStatus {
        message_id: String,
        persistence: CompactPersistence,
    },
    MessageRemove {
        message_id: String,
    },
}

impl CompactColdOperation {
    pub fn message_id(&self) -> &str {
        match self {
            Self::MessageStatus { message_id, .. } | Self::MessageRemove { message_id } => {
                message_id
            }
        }
    }

    pub fn from_transition(transition: &CompactTransition) -> Option<Self> {
        match transition {
            CompactTransition::MessageStatus {
                message_id,
                persistence,
            } => Some(Self::MessageStatus {
                message_id: message_id.clone(),
                persistence: *persistence,
            }),
            CompactTransition::MessageRemove { message_id } => Some(Self::MessageRemove {
                message_id: message_id.clone(),
            }),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColdRejection {
    UnknownMessage,
    ActiveMessage,
    Capacity,
    Retired,
}

#[derive(Debug, Clone)]
pub enum CompactColdOutcome {
    Applied {
        after_sequence: u64,
    },
    Rejected {
        after_sequence: u64,
        reason: ColdRejection,
    },
    Failed {
        after_sequence: u64,
        cause: CompactError,
    },
}

struct Token {
    removed: bool,
    active: bool,
    pending: usize,
}

type IsActive = Arc<dyn Fn(&str) -> bool + Send + Sync>;

pub struct CompactColdOperations<Row> {
    index: CompactContentIndex<Row>,
    is_active: IsActive,
    /// FIFO tickets taken at admission: `(next to admit, now serving)`.
    tickets: Mutex<(u64, u64)>,
    served: tokio::sync::Notify,
    keys: Mutex<HashMap<String, Arc<Mutex<Token>>>>,
    pending: Mutex<usize>,
}

impl<Row> CompactColdOperations<Row>
where
    Row: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    pub fn new(
        index: CompactContentIndex<Row>,
        is_active: impl Fn(&str) -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            index,
            is_active: Arc::new(is_active),
            tickets: Mutex::new((0, 0)),
            served: tokio::sync::Notify::new(),
            keys: Mutex::new(HashMap::new()),
            pending: Mutex::new(0),
        }
    }

    pub fn activate(&self, key: &str) {
        if let Some(token) = lock(&self.keys).get(key) {
            lock(token).active = true;
        }
    }

    /// Admits one operation synchronously (the returned future then runs it in FIFO order).
    pub fn run(
        &self,
        operation: CompactColdOperation,
        after_sequence: u64,
    ) -> impl std::future::Future<Output = CompactColdOutcome> + '_ {
        let key = operation.message_id().to_owned();
        let rejected = |reason| CompactColdOutcome::Rejected {
            after_sequence,
            reason,
        };
        let admitted = if (self.is_active)(&key) {
            Err(rejected(ColdRejection::ActiveMessage))
        } else if *lock(&self.pending) == 128 {
            // Bound queued roots, keys and closures independently of the settled membership catalog.
            Err(rejected(ColdRejection::Capacity))
        } else {
            let mut keys = lock(&self.keys);
            let reuse = keys.get(&key).filter(|token| !lock(token).active).cloned();
            let token = reuse.unwrap_or_else(|| {
                Arc::new(Mutex::new(Token {
                    removed: false,
                    active: false,
                    pending: 0,
                }))
            });
            keys.insert(key.clone(), token.clone());
            lock(&token).pending += 1;
            *lock(&self.pending) += 1;
            Ok(token)
        };
        // The FIFO position is taken at admission, before any later `run` call.
        let ticket = admitted.as_ref().ok().map(|_| {
            let mut tickets = lock(&self.tickets);
            tickets.0 += 1;
            tickets.0 - 1
        });
        async move {
            let token = match admitted {
                Ok(token) => token,
                Err(outcome) => return outcome,
            };
            let ticket = ticket.unwrap_or_default();
            self.wait_for(|serving| serving == ticket).await;
            let outcome = self.apply(&operation, &key, &token, after_sequence).await;
            lock(&self.tickets).1 += 1;
            self.served.notify_waiters();
            *lock(&self.pending) -= 1;
            let remaining = {
                let mut token = lock(&token);
                token.pending -= 1;
                token.pending
            };
            let mut keys = lock(&self.keys);
            if remaining == 0
                && keys
                    .get(&key)
                    .is_some_and(|current| Arc::ptr_eq(current, &token))
            {
                keys.remove(&key);
            }
            outcome
        }
    }

    async fn apply(
        &self,
        operation: &CompactColdOperation,
        key: &str,
        token: &Arc<Mutex<Token>>,
        after_sequence: u64,
    ) -> CompactColdOutcome {
        let rejected = |reason| CompactColdOutcome::Rejected {
            after_sequence,
            reason,
        };
        let result = async {
            self.index.store().assert_readable()?;
            let active = |token: &Arc<Mutex<Token>>| lock(token).active || (self.is_active)(key);
            if active(token) {
                return Ok(rejected(ColdRejection::ActiveMessage));
            }
            if lock(token).removed {
                return Ok(rejected(ColdRejection::UnknownMessage));
            }
            let cut = self.index.freeze()?;
            let applied = async {
                let row = self.index.lookup(&cut, key).await?;
                self.index.store().assert_readable()?;
                if active(token) {
                    return Ok(rejected(ColdRejection::ActiveMessage));
                }
                if row.is_none() {
                    return Ok(rejected(ColdRejection::UnknownMessage));
                }
                match operation {
                    CompactColdOperation::MessageStatus { persistence, .. } => self.index.patch(
                        key,
                        &[("persistence".into(), serde_json::to_value(persistence)?)],
                    )?,
                    CompactColdOperation::MessageRemove { .. } => {
                        self.index.delete(key)?;
                        lock(token).removed = true;
                        lock(&self.keys).remove(key);
                    }
                }
                Ok(CompactColdOutcome::Applied { after_sequence })
            }
            .await;
            self.index.release(&cut);
            applied
        }
        .await;
        match result {
            Ok(outcome) => outcome,
            Err(cause) if cause.is_retired() => rejected(ColdRejection::Retired),
            Err(cause) => CompactColdOutcome::Failed {
                after_sequence,
                cause,
            },
        }
    }

    async fn wait_for(&self, ready: impl Fn(u64) -> bool) {
        loop {
            let notified = self.served.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if ready(lock(&self.tickets).1) {
                return;
            }
            notified.await;
        }
    }

    /// Resolves once every operation admitted before this call has run.
    pub async fn idle(&self) {
        let admitted = lock(&self.tickets).0;
        self.wait_for(|serving| serving >= admitted).await;
    }
}
