//! Port of senpi packages/coding-agent/src/core/session-move/move-ledger.ts.
//!
//! The receiving device's record of every move it was offered. Asking about a move is itself a fence: a move
//! this device never heard of is recorded as declined BEFORE the answer leaves, so a later import of it is
//! refused and the sender may safely take its session back. Every writer and the import's final rename share
//! one lock per move.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};

use super::bundle::{MoveId, parse_move_id};
use crate::lockfile_policy::acquire_lock_async;
use crate::session_sidecar_store::serialize_by_key;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclineReason {
    NoRecord,
    Expired,
    Cancelled,
    ChunkWithoutReady,
    Repository,
}

impl DeclineReason {
    const ALL: [Self; 5] = [
        Self::NoRecord,
        Self::Expired,
        Self::Cancelled,
        Self::ChunkWithoutReady,
        Self::Repository,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoRecord => "no_record",
            Self::Expired => "expired",
            Self::Cancelled => "cancelled",
            Self::ChunkWithoutReady => "chunk_without_ready",
            Self::Repository => "repository",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailReason {
    BundleInvalid,
    ImportFailed,
}

impl FailReason {
    const ALL: [Self; 2] = [Self::BundleInvalid, Self::ImportFailed];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BundleInvalid => "bundle_invalid",
            Self::ImportFailed => "import_failed",
        }
    }
}

/// The answers a sender can hear.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoveStatus {
    Receiving,
    Accepted,
    Declined { reason: DeclineReason },
    Failed { reason: FailReason },
}

/// `ready`: this device told the sender it can take the move; no bundle has been verified yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoveRecord {
    Status(MoveStatus),
    Ready { expires_at: String },
}

impl MoveRecord {
    pub fn state(&self) -> &'static str {
        match self {
            Self::Ready { .. } => "ready",
            Self::Status(MoveStatus::Receiving) => "receiving",
            Self::Status(MoveStatus::Accepted) => "accepted",
            Self::Status(MoveStatus::Declined { .. }) => "declined",
            Self::Status(MoveStatus::Failed { .. }) => "failed",
        }
    }

    fn is_final(&self) -> bool {
        matches!(
            self,
            Self::Status(
                MoveStatus::Accepted | MoveStatus::Declined { .. } | MoveStatus::Failed { .. }
            )
        )
    }

    fn to_json(&self) -> Value {
        match self {
            Self::Ready { expires_at } => json!({"state": "ready", "expiresAt": expires_at}),
            Self::Status(MoveStatus::Receiving) => json!({"state": "receiving"}),
            Self::Status(MoveStatus::Accepted) => json!({"state": "accepted"}),
            Self::Status(MoveStatus::Declined { reason }) => {
                json!({"state": "declined", "reason": reason.as_str()})
            }
            Self::Status(MoveStatus::Failed { reason }) => {
                json!({"state": "failed", "reason": reason.as_str()})
            }
        }
    }
}

/// How long a ready move waits for its bundle before it is declined as expired.
pub const READY_TTL_MS: i64 = 10 * 60_000;
const LOCK_WAIT_MS: u64 = 15_000;

/// `MoveFencedError` (`code: "move_fenced"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveFencedError {
    pub move_id: MoveId,
    pub record: MoveRecord,
}

impl MoveFencedError {
    pub const CODE: &'static str = "move_fenced";
}

impl std::fmt::Display for MoveFencedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Move {} can no longer be imported here: it was already answered ({}).",
            self.move_id,
            self.record.state()
        )
    }
}

impl std::error::Error for MoveFencedError {}

#[derive(Debug)]
pub enum MoveLedgerError {
    Fenced(MoveFencedError),
    Failed(String),
}

impl std::fmt::Display for MoveLedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fenced(error) => error.fmt(f),
            Self::Failed(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for MoveLedgerError {}

impl From<MoveFencedError> for MoveLedgerError {
    fn from(error: MoveFencedError) -> Self {
        Self::Fenced(error)
    }
}

impl From<std::io::Error> for MoveLedgerError {
    fn from(error: std::io::Error) -> Self {
        Self::Failed(error.to_string())
    }
}

fn parse_record(value: &Value) -> Option<MoveRecord> {
    let state = value.get("state")?.as_str()?;
    let reason = value.get("reason").and_then(Value::as_str);
    match state {
        "receiving" => Some(MoveRecord::Status(MoveStatus::Receiving)),
        "accepted" => Some(MoveRecord::Status(MoveStatus::Accepted)),
        "ready" => {
            let expires_at = value.get("expiresAt")?.as_str()?;
            chrono::DateTime::parse_from_rfc3339(expires_at).ok()?;
            Some(MoveRecord::Ready {
                expires_at: expires_at.to_owned(),
            })
        }
        "declined" => DeclineReason::ALL
            .into_iter()
            .find(|known| Some(known.as_str()) == reason)
            .map(|reason| MoveRecord::Status(MoveStatus::Declined { reason })),
        "failed" => FailReason::ALL
            .into_iter()
            .find(|known| Some(known.as_str()) == reason)
            .map(|reason| MoveRecord::Status(MoveStatus::Failed { reason })),
        _ => None,
    }
}

type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

fn system_now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub struct MoveLedger {
    dir: PathBuf,
    now: Clock,
}

impl MoveLedger {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            now: Arc::new(system_now_ms),
        }
    }

    /// An injectable clock (milliseconds since the epoch), for tests.
    pub fn with_now(
        dir: impl Into<PathBuf>,
        now: impl Fn() -> i64 + Send + Sync + 'static,
    ) -> Self {
        Self {
            dir: dir.into(),
            now: Arc::new(now),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Says this device can take the move, for `READY_TTL_MS`. Sending ready again keeps the first window; a move
    /// already receiving or accepted stays so, and one already answered otherwise is refused.
    pub async fn prepare(&self, move_id: &str) -> Result<MoveRecord, MoveLedgerError> {
        self.locked(move_id, || {
            let Some(record) = self.current(move_id)? else {
                let expires = chrono::DateTime::from_timestamp_millis((self.now)() + READY_TTL_MS)
                    .unwrap_or_default();
                let ready = MoveRecord::Ready {
                    expires_at: expires.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                };
                self.write(move_id, &ready)?;
                return Ok(ready);
            };
            match record {
                MoveRecord::Ready { .. }
                | MoveRecord::Status(MoveStatus::Receiving | MoveStatus::Accepted) => Ok(record),
                other => Err(MoveFencedError {
                    move_id: move_id.to_owned(),
                    record: other,
                }
                .into()),
            }
        })
        .await
    }

    /// Starts receiving a verified bundle. Only a move this device readied (and whose window is still open) may
    /// start; a bundle for a move it never readied is declined for good (`chunk_without_ready`).
    pub async fn begin_receive(&self, move_id: &str) -> Result<(), MoveLedgerError> {
        self.locked(move_id, || match self.current(move_id)? {
            Some(MoveRecord::Status(MoveStatus::Accepted | MoveStatus::Receiving)) => Ok(()),
            Some(MoveRecord::Ready { .. }) => {
                Ok(self.write(move_id, &MoveRecord::Status(MoveStatus::Receiving))?)
            }
            Some(other) => Err(MoveFencedError {
                move_id: move_id.to_owned(),
                record: other,
            }
            .into()),
            None => {
                let declined = MoveRecord::Status(MoveStatus::Declined {
                    reason: DeclineReason::ChunkWithoutReady,
                });
                self.write(move_id, &declined)?;
                Err(MoveFencedError {
                    move_id: move_id.to_owned(),
                    record: declined,
                }
                .into())
            }
        })
        .await
    }

    /// Declines a ready move whose window has passed; any other record is returned unchanged.
    pub async fn expire(&self, move_id: &str) -> Result<Option<MoveRecord>, MoveLedgerError> {
        self.locked(move_id, || self.current(move_id)).await
    }

    /// The signed answer a sender gets; `moved_in` is whether the session file here already records the move.
    pub async fn answer_status(
        &self,
        move_id: &str,
        moved_in: bool,
    ) -> Result<MoveStatus, MoveLedgerError> {
        self.locked(move_id, || {
            let record = self.current(move_id)?;
            if moved_in || matches!(record, Some(MoveRecord::Status(MoveStatus::Accepted))) {
                return Ok(MoveStatus::Accepted);
            }
            match record {
                Some(MoveRecord::Ready { .. }) => Ok(MoveStatus::Receiving),
                Some(MoveRecord::Status(status)) => Ok(status),
                None => {
                    let declined = MoveStatus::Declined {
                        reason: DeclineReason::NoRecord,
                    };
                    self.write(move_id, &MoveRecord::Status(declined.clone()))?;
                    Ok(declined)
                }
            }
        })
        .await
    }

    /// Cancel, expiry or a refused chunk: only before the bundle was verified and receiving began, never after.
    pub async fn decline(
        &self,
        move_id: &str,
        reason: DeclineReason,
    ) -> Result<MoveRecord, MoveLedgerError> {
        self.settle(
            move_id,
            MoveRecord::Status(MoveStatus::Declined { reason }),
            &["receiving"],
        )
        .await
    }

    /// A verified import that could not finish; it may end a receiving move but never an accepted one.
    pub async fn fail(
        &self,
        move_id: &str,
        reason: FailReason,
    ) -> Result<MoveRecord, MoveLedgerError> {
        self.settle(
            move_id,
            MoveRecord::Status(MoveStatus::Failed { reason }),
            &[],
        )
        .await
    }

    /// Runs the import's commit (the atomic rename) under the move's lock, then records it accepted.
    pub async fn commit<T>(
        &self,
        move_id: &str,
        rename: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, MoveLedgerError> {
        self.locked(move_id, || {
            if let Some(record) = self.read(move_id)?
                && !matches!(
                    record,
                    MoveRecord::Status(MoveStatus::Receiving | MoveStatus::Accepted)
                )
            {
                return Err(MoveFencedError {
                    move_id: move_id.to_owned(),
                    record,
                }
                .into());
            }
            let result = rename().map_err(MoveLedgerError::Failed)?;
            self.write(move_id, &MoveRecord::Status(MoveStatus::Accepted))?;
            Ok(result)
        })
        .await
    }

    /// Records a final answer unless one is already there: once a sender may have heard `accepted`, `declined` or
    /// `failed`, it never changes. Reads through `current()`, so a ready move whose window passed answers
    /// `expired`. `keeps` names the open states this transition must also leave alone.
    async fn settle(
        &self,
        move_id: &str,
        next: MoveRecord,
        keeps: &[&str],
    ) -> Result<MoveRecord, MoveLedgerError> {
        self.locked(move_id, || {
            if let Some(record) = self.current(move_id)?
                && (record.is_final() || keeps.contains(&record.state()))
            {
                return Ok(record);
            }
            self.write(move_id, &next)?;
            Ok(next.clone())
        })
        .await
    }

    /// The record, with a ready move whose window has passed declined as expired first (on disk, under the lock).
    fn current(&self, move_id: &str) -> Result<Option<MoveRecord>, MoveLedgerError> {
        let record = self.read(move_id)?;
        let Some(MoveRecord::Ready { expires_at }) = &record else {
            return Ok(record);
        };
        let expires_ms = chrono::DateTime::parse_from_rfc3339(expires_at)
            .map_or(i64::MIN, |at| at.timestamp_millis());
        if (self.now)() < expires_ms {
            return Ok(record);
        }
        let expired = MoveRecord::Status(MoveStatus::Declined {
            reason: DeclineReason::Expired,
        });
        self.write(move_id, &expired)?;
        Ok(Some(expired))
    }

    fn path(&self, move_id: &str) -> Result<PathBuf, MoveLedgerError> {
        let id = parse_move_id(&Value::String(move_id.to_owned()))
            .map_err(|error| MoveLedgerError::Failed(error.to_string()))?;
        Ok(self.dir.join(format!("{id}.json")))
    }

    fn read(&self, move_id: &str) -> Result<Option<MoveRecord>, MoveLedgerError> {
        let path = self.path(move_id)?;
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let parsed: Value = serde_json::from_str(&raw)
            .map_err(|error| MoveLedgerError::Failed(error.to_string()))?;
        parse_record(&parsed).map(Some).ok_or_else(|| {
            MoveLedgerError::Failed(format!(
                "Move record {} is not a move record",
                path.display()
            ))
        })
    }

    fn write(&self, move_id: &str, record: &MoveRecord) -> Result<(), MoveLedgerError> {
        let file = self.path(move_id)?;
        let temp = PathBuf::from(format!("{}.{}.tmp", file.display(), std::process::id()));
        let mut body = record.to_json();
        body["at"] =
            Value::String(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
        std::fs::write(&temp, format!("{body}\n"))?;
        std::fs::rename(&temp, &file)?;
        Ok(())
    }

    async fn locked<T>(
        &self,
        move_id: &str,
        work: impl FnOnce() -> Result<T, MoveLedgerError>,
    ) -> Result<T, MoveLedgerError> {
        let file = self.path(move_id)?;
        let key = file.to_string_lossy().into_owned();
        serialize_by_key(&key, async {
            std::fs::create_dir_all(&self.dir)?;
            let _guard = acquire_lock_async(&key, LOCK_WAIT_MS)
                .await
                .map_err(|error| {
                    MoveLedgerError::Failed(format!(
                        "Move record {key} stayed locked for {}ms",
                        error.waited_ms
                    ))
                })?;
            work()
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};

    fn ledger(dir: &Path, clock: &Arc<AtomicI64>) -> MoveLedger {
        let clock = Arc::clone(clock);
        MoveLedger::with_now(dir, move || clock.load(Ordering::SeqCst))
    }

    fn run<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime")
            .block_on(future)
    }

    #[test]
    fn prepare_keeps_the_first_window_and_begin_receive_needs_ready() {
        run(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let clock = Arc::new(AtomicI64::new(1_000_000));
            let ledger = ledger(dir.path(), &clock);
            let first = ledger.prepare("m1").await.expect("prepare");
            clock.store(1_000_500, Ordering::SeqCst);
            assert_eq!(ledger.prepare("m1").await.expect("again"), first);
            ledger.begin_receive("m1").await.expect("receive");
            assert_eq!(
                ledger.prepare("m1").await.expect("receiving stays"),
                MoveRecord::Status(MoveStatus::Receiving)
            );
            let unknown = ledger.begin_receive("m2").await.expect_err("no ready");
            assert!(
                matches!(unknown, MoveLedgerError::Fenced(ref fenced) if fenced.record == MoveRecord::Status(MoveStatus::Declined { reason: DeclineReason::ChunkWithoutReady }))
            );
        });
    }

    #[test]
    fn an_elapsed_ready_window_is_declined_as_expired_on_disk() {
        run(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let clock = Arc::new(AtomicI64::new(0));
            let ledger = ledger(dir.path(), &clock);
            ledger.prepare("m1").await.expect("prepare");
            clock.store(READY_TTL_MS + 1, Ordering::SeqCst);
            assert_eq!(
                ledger.expire("m1").await.expect("expire"),
                Some(MoveRecord::Status(MoveStatus::Declined {
                    reason: DeclineReason::Expired
                }))
            );
            let stored = std::fs::read_to_string(dir.path().join("m1.json")).expect("record");
            assert!(stored.contains("\"expired\""));
            assert!(matches!(
                ledger.prepare("m1").await,
                Err(MoveLedgerError::Fenced(_))
            ));
        });
    }

    #[test]
    fn answer_status_maps_ready_to_receiving_and_fences_unknown_moves() {
        run(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let clock = Arc::new(AtomicI64::new(0));
            let ledger = ledger(dir.path(), &clock);
            ledger.prepare("m1").await.expect("prepare");
            assert_eq!(
                ledger.answer_status("m1", false).await.expect("answer"),
                MoveStatus::Receiving
            );
            assert_eq!(
                ledger.answer_status("m3", false).await.expect("answer"),
                MoveStatus::Declined {
                    reason: DeclineReason::NoRecord
                }
            );
            assert_eq!(
                ledger.answer_status("m3", true).await.expect("moved in"),
                MoveStatus::Accepted
            );
        });
    }

    #[test]
    fn settle_never_changes_a_final_state_and_keeps_receiving_for_decline_only() {
        run(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let clock = Arc::new(AtomicI64::new(0));
            let ledger = ledger(dir.path(), &clock);
            ledger.prepare("m1").await.expect("prepare");
            ledger.begin_receive("m1").await.expect("receive");
            assert_eq!(
                ledger
                    .decline("m1", DeclineReason::Cancelled)
                    .await
                    .expect("decline"),
                MoveRecord::Status(MoveStatus::Receiving)
            );
            assert_eq!(
                ledger
                    .fail("m1", FailReason::ImportFailed)
                    .await
                    .expect("fail"),
                MoveRecord::Status(MoveStatus::Failed {
                    reason: FailReason::ImportFailed
                })
            );
            assert_eq!(
                ledger
                    .decline("m1", DeclineReason::Cancelled)
                    .await
                    .expect("final"),
                MoveRecord::Status(MoveStatus::Failed {
                    reason: FailReason::ImportFailed
                })
            );
        });
    }

    #[test]
    fn commit_records_accepted_and_refuses_answered_moves() {
        run(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let clock = Arc::new(AtomicI64::new(0));
            let ledger = ledger(dir.path(), &clock);
            ledger.prepare("m1").await.expect("prepare");
            ledger.begin_receive("m1").await.expect("receive");
            assert_eq!(ledger.commit("m1", || Ok(7)).await.expect("commit"), 7);
            assert_eq!(
                ledger.answer_status("m1", false).await.expect("answer"),
                MoveStatus::Accepted
            );
            ledger
                .decline("m4", DeclineReason::Cancelled)
                .await
                .expect("decline");
            assert!(matches!(
                ledger.commit("m4", || Ok(())).await,
                Err(MoveLedgerError::Fenced(_))
            ));
        });
    }
}
