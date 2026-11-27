//! Port of senpi packages/coding-agent/src/core/session-move/move-ledger.ts.
//!
//! The receiving device's record of every move it was offered. Asking about a move is itself a fence: a move
//! this device never heard of is recorded as declined BEFORE the answer leaves, so a later import of it is
//! refused and the sender may safely take its session back. Every writer and the import's final rename share
//! one lock per move.

use super::move_landing::{
    Landing, LandingOutcome, StampClock, held_elsewhere, landed_elsewhere, lands_at, move_folder,
    parse_landing, settled_at, settled_landing_with_age, stamp_age, stamp_fields,
};
use super::move_ledger_lock::MoveRecordLocks;
use super::move_record_file::MoveRecordFile;
use super::move_steps::{MoveStep, move_step};
use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

/// Reads what the session file at a landing target records for the move; it may wait on I/O.
pub type LandedReader<'a> =
    &'a (dyn Fn(String) -> Pin<Box<dyn Future<Output = LandingOutcome> + Send>> + Send + Sync);

use serde_json::{Value, json};

use super::bundle::{MoveId, parse_move_id};
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
    AcceptedRevoked,
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
            Self::Status(MoveStatus::Accepted | MoveStatus::AcceptedRevoked) => "accepted",
            Self::Status(MoveStatus::Declined { .. }) => "declined",
            Self::Status(MoveStatus::Failed { .. }) => "failed",
        }
    }

    fn is_final(&self) -> bool {
        matches!(
            self,
            Self::Status(
                MoveStatus::Accepted
                    | MoveStatus::AcceptedRevoked
                    | MoveStatus::Declined { .. }
                    | MoveStatus::Failed { .. }
            )
        )
    }

    pub fn to_json(&self) -> Value {
        match self {
            Self::Ready { expires_at } => json!({"state": "ready", "expiresAt": expires_at}),
            Self::Status(MoveStatus::Receiving) => json!({"state": "receiving"}),
            Self::Status(MoveStatus::Accepted) => json!({"state": "accepted"}),
            Self::Status(MoveStatus::AcceptedRevoked) => {
                json!({"state": "accepted", "revoked": true})
            }
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

/// `MoveFencedError` (`code: "move_fenced"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveFencedError {
    pub move_id: MoveId,
    pub record: MoveRecord,
    pub detail: Option<String>,
}

impl MoveFencedError {
    pub const CODE: &'static str = "move_fenced";

    pub fn new(move_id: impl Into<String>, record: MoveRecord, detail: Option<String>) -> Self {
        Self {
            move_id: move_id.into(),
            record,
            detail,
        }
    }

    pub fn with_detail(
        move_id: impl Into<String>,
        record: Option<MoveRecord>,
        detail: Option<String>,
    ) -> MoveLedgerError {
        let move_id = move_id.into();
        match record {
            Some(record) => MoveFencedError {
                move_id,
                record,
                detail,
            }
            .into(),
            None => MoveLedgerError::Failed(format!(
                "move_fenced: Move {move_id} can no longer be imported here: {}.",
                detail.unwrap_or_else(|| "it was already answered (no record)".to_owned())
            )),
        }
    }
}

impl std::fmt::Display for MoveFencedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.detail {
            Some(detail) => write!(
                f,
                "Move {} can no longer be imported here: {detail}.",
                self.move_id
            ),
            None => write!(
                f,
                "Move {} can no longer be imported here: it was already answered ({}).",
                self.move_id,
                self.record.state()
            ),
        }
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

pub type MoveAnswer = MoveStatus;

/// Parse only receiver answers; a ready record is never a signed status answer.
pub fn parse_move_status(value: &Value) -> Option<MoveAnswer> {
    match parse_record(value)? {
        MoveRecord::Status(status) => Some(status),
        MoveRecord::Ready { .. } => None,
    }
}

pub fn parse_record(value: &Value) -> Option<MoveRecord> {
    let state = value.get("state")?.as_str()?;
    let reason = value.get("reason").and_then(Value::as_str);
    match state {
        "receiving" => Some(MoveRecord::Status(MoveStatus::Receiving)),
        "accepted" if value.get("revoked").and_then(Value::as_bool) == Some(true) => {
            Some(MoveRecord::Status(MoveStatus::AcceptedRevoked))
        }
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

pub struct MoveLedger {
    dir: PathBuf,
    clock: StampClock,
    locks: Arc<MoveRecordLocks>,
}

impl MoveLedger {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self::with_clock(dir, StampClock::default())
    }

    pub fn with_clock(dir: impl Into<PathBuf>, clock: StampClock) -> Self {
        Self {
            dir: dir.into(),
            clock,
            locks: Arc::new(MoveRecordLocks::default()),
        }
    }

    /// An injectable clock (milliseconds since the epoch), for tests.
    pub fn with_now(
        dir: impl Into<PathBuf>,
        now: impl Fn() -> i64 + Send + Sync + 'static,
    ) -> Self {
        Self::with_clock(
            dir,
            StampClock {
                now: Arc::new(now),
                ..StampClock::default()
            },
        )
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn stamp(&self) -> BTreeMap<String, Value> {
        stamp_fields(&self.clock)
    }

    fn record_file<'a>(
        &'a self,
        stamp_fn: &'a (dyn Fn() -> BTreeMap<String, Value> + Send + Sync),
    ) -> MoveRecordFile<'a> {
        MoveRecordFile::new(&self.dir, &self.locks, Some(stamp_fn))
    }

    /// Says this device can take the move, for `READY_TTL_MS`. Sending ready again keeps the first window; a move
    /// already receiving or accepted stays so, and one already answered otherwise is refused.
    pub async fn prepare(&self, move_id: &str) -> Result<MoveRecord, MoveLedgerError> {
        self.prepare_for_cwd(move_id, None).await
    }

    pub async fn prepare_for_cwd(
        &self,
        move_id: &str,
        folder: Option<&str>,
    ) -> Result<MoveRecord, MoveLedgerError> {
        let cwd = move_folder(folder);
        self.locked(move_id, || {
            let stamp_fn = || self.stamp();
            let file = self.record_file(&stamp_fn);
            let record = self.current(move_id)?;
            let landing = file.landing(move_id)?;
            if let (Some(c), Some(l)) = (cwd.as_deref(), landing.cwd.as_deref())
                && l != c
            {
                return Err(MoveFencedError::with_detail(
                    move_id,
                    record,
                    Some(held_elsewhere(&landing)),
                ));
            }
            let Some(record) = record else {
                let expires =
                    chrono::DateTime::from_timestamp_millis((self.clock.now)() + READY_TTL_MS)
                        .unwrap_or_default();
                let ready = MoveRecord::Ready {
                    expires_at: expires.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                };
                let initial = Landing {
                    cwd: cwd.clone(),
                    ..Landing::default()
                };
                file.write(move_id, &ready, &initial)?;
                return Ok(ready);
            };
            match &record {
                MoveRecord::Ready { .. }
                | MoveRecord::Status(
                    MoveStatus::Receiving | MoveStatus::Accepted | MoveStatus::AcceptedRevoked,
                ) => {
                    if let Some(c) = cwd.as_deref()
                        && landing.cwd.is_none()
                        && !landing.committing
                    {
                        let mut updated = landing;
                        updated.cwd = Some(c.to_owned());
                        file.write(move_id, &record, &updated)?;
                    }
                    Ok(record)
                }
                _ => Err(MoveFencedError::new(move_id, record, None).into()),
            }
        })
        .await
    }

    /// Refuses an import into a folder other than the one a ready move was readied for, without taking a lock.
    pub async fn require_readied_for(
        &self,
        move_id: &str,
        cwd: &str,
    ) -> Result<(), MoveLedgerError> {
        let stamp_fn = || self.stamp();
        let file = self.record_file(&stamp_fn);
        let Some(stored) = file.raw(move_id)? else {
            return Ok(());
        };
        let record = parse_record(&stored);
        let landing = parse_landing(&stored);
        let expected = move_folder(Some(cwd));
        if matches!(record, Some(MoveRecord::Ready { .. }))
            && landing.cwd.is_some()
            && landing.cwd != expected
        {
            return Err(MoveFencedError::with_detail(
                move_id,
                record,
                Some(held_elsewhere(&landing)),
            ));
        }
        Ok(())
    }

    /// Starts receiving a verified bundle. Only a move this device readied (and whose window is still open) may
    /// start; a bundle for a move it never readied is declined for good (`chunk_without_ready`).
    pub async fn begin_receive(&self, move_id: &str, target: &str) -> Result<(), MoveLedgerError> {
        self.begin_receive_with_cwd(move_id, target, None).await
    }

    pub async fn begin_receive_with_cwd(
        &self,
        move_id: &str,
        target: &str,
        folder: Option<&str>,
    ) -> Result<(), MoveLedgerError> {
        let cwd = move_folder(folder);
        self.locked(move_id, || {
            let stamp_fn = || self.stamp();
            let file = self.record_file(&stamp_fn);
            let record = self.current(move_id)?;
            let landing = file.landing(move_id)?;
            let receiving = lands_at(record.as_ref(), &landing, target);
            if (matches!(record, Some(MoveRecord::Ready { .. })) || receiving)
                && landing.cwd.is_some()
                && landing.cwd != cwd
            {
                return Err(MoveFencedError::with_detail(
                    move_id,
                    record,
                    Some(held_elsewhere(&landing)),
                ));
            }
            if matches!(record, Some(MoveRecord::Ready { .. })) {
                let next_landing = settled_at(
                    &Landing {
                        cwd: cwd.clone(),
                        ..Landing::default()
                    },
                    target,
                );
                file.write(
                    move_id,
                    &MoveRecord::Status(MoveStatus::Receiving),
                    &next_landing,
                )?;
                return Ok(());
            }
            if receiving {
                return Ok(());
            }
            if matches!(
                record,
                Some(MoveRecord::Status(
                    MoveStatus::Receiving | MoveStatus::Accepted | MoveStatus::AcceptedRevoked
                ))
            ) {
                let detail = landed_elsewhere(record.as_ref(), &landing);
                return Err(MoveFencedError::with_detail(move_id, record, Some(detail)));
            }
            if let Some(record) = record {
                return Err(MoveFencedError::new(move_id, record, None).into());
            }
            let declined = MoveRecord::Status(MoveStatus::Declined {
                reason: DeclineReason::ChunkWithoutReady,
            });
            file.write(move_id, &declined, &Landing::default())?;
            Err(MoveFencedError::new(move_id, declined, None).into())
        })
        .await
    }

    pub fn assert_held(&self, move_id: &str) -> Result<(), MoveLedgerError> {
        let path = self.path(move_id)?;
        self.locks
            .assert_held(&path)
            .map_err(MoveLedgerError::Failed)
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
        self.answer_status_with(move_id, moved_in, false, None)
            .await
    }

    pub async fn answer_status_with(
        &self,
        move_id: &str,
        moved_in: bool,
        revoked: bool,
        landed: Option<LandedReader<'_>>,
    ) -> Result<MoveStatus, MoveLedgerError> {
        self.locked_async(move_id, async {
            let stamp_fn = || self.stamp();
            let file = self.record_file(&stamp_fn);
            let mut record = self.current(move_id)?;
            let stored = file.raw(move_id)?;
            let landing = stored.as_ref().map(parse_landing).unwrap_or_default();
            let outcome = match (landed, landing.target.as_deref()) {
                (Some(read), Some(target)) => Some(read(target.to_owned()).await),
                _ => None,
            };
            if let (Some(outcome), Some(target)) = (outcome, landing.target.as_deref()) {
                let age = stored.as_ref().and_then(|v| stamp_age(v, &self.clock));
                if let Some(next) =
                    settled_landing_with_age(record.as_ref(), landing.committing, outcome, age)
                {
                    file.write(
                        move_id,
                        &next,
                        &settled_at(
                            &Landing {
                                cwd: landing.cwd.clone(),
                                ..Landing::default()
                            },
                            target,
                        ),
                    )?;
                    record = Some(next);
                }
            }
            if revoked {
                return Ok(MoveStatus::AcceptedRevoked);
            }
            if let Some(MoveRecord::Status(
                status @ (MoveStatus::Accepted | MoveStatus::AcceptedRevoked),
            )) = &record
            {
                return Ok(status.clone());
            }
            if moved_in {
                return Ok(MoveStatus::Accepted);
            }
            match record {
                Some(MoveRecord::Ready { .. }) => Ok(MoveStatus::Receiving),
                Some(MoveRecord::Status(status)) => Ok(status),
                None => {
                    let declined = MoveStatus::Declined {
                        reason: DeclineReason::NoRecord,
                    };
                    file.write(
                        move_id,
                        &MoveRecord::Status(declined.clone()),
                        &Landing::default(),
                    )?;
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
        target: &str,
        rename: impl Future<Output = Result<T, String>>,
        prepare: impl Future<Output = Result<(), String>>,
    ) -> Result<T, MoveLedgerError> {
        self.commit_with_stamp(move_id, target, |_| rename, prepare)
            .await
    }

    /// Runs the commit giving `rename` a callback that verifies this commit still owns its stamp
    /// and lock right before landing.
    pub async fn commit_with_stamp<T, R, RFut>(
        &self,
        move_id: &str,
        target: &str,
        rename: R,
        prepare: impl Future<Output = Result<(), String>>,
    ) -> Result<T, MoveLedgerError>
    where
        R: FnOnce(Arc<dyn Fn() -> Result<(), MoveLedgerError> + Send + Sync>) -> RFut,
        RFut: Future<Output = Result<T, String>>,
    {
        self.locked_async(move_id, async {
            let stamp_fn = || self.stamp();
            let file = self.record_file(&stamp_fn);
            file.assert_lands_at(move_id, target)?;
            // `prepare` runs before the stamp: whatever it does or refuses lands nothing.
            prepare.await.map_err(MoveLedgerError::Failed)?;
            let landing = file.assert_lands_at(move_id, target)?;
            let mut committing_landing = settled_at(&landing, target);
            committing_landing.committing = true;
            let stamp = file.write(
                move_id,
                &MoveRecord::Status(MoveStatus::Receiving),
                &committing_landing,
            )?;
            move_step(MoveStep::ImportStamped).map_err(MoveLedgerError::Failed)?;
            file.assert_stamp_stands(move_id, &stamp)?;
            self.locks
                .assert_held(&file.path(move_id)?)
                .map_err(MoveLedgerError::Failed)?;
            let stamp_stands: Arc<dyn Fn() -> Result<(), MoveLedgerError> + Send + Sync> = {
                let dir = self.dir.clone();
                let locks = Arc::clone(&self.locks);
                let move_id_owned = move_id.to_owned();
                let stamp_owned = stamp.clone();
                Arc::new(move || {
                    let rf = MoveRecordFile::new(&dir, &locks, None);
                    rf.assert_stamp_stands(&move_id_owned, &stamp_owned)
                })
            };
            let result = rename(stamp_stands)
                .await
                .map_err(MoveLedgerError::Failed)?;
            move_step(MoveStep::ImportRenamed).map_err(MoveLedgerError::Failed)?;
            file.write(
                move_id,
                &MoveRecord::Status(MoveStatus::Accepted),
                &settled_at(&landing, target),
            )?;
            Ok(result)
        })
        .await
    }

    pub async fn confirm_landed(
        &self,
        move_id: &str,
        target: &str,
        revoked: bool,
    ) -> Result<(), MoveLedgerError> {
        self.locked(move_id, || {
            let stamp_fn = || self.stamp();
            let file = self.record_file(&stamp_fn);
            let record = file.read(move_id)?;
            let landing = file.landing(move_id)?;
            if record.as_ref().is_some_and(|r| {
                !matches!(
                    r,
                    MoveRecord::Status(
                        MoveStatus::Receiving | MoveStatus::Accepted | MoveStatus::AcceptedRevoked
                    )
                )
            }) || landing
                .target
                .as_deref()
                .is_some_and(|stored| stored != target)
            {
                let detail = landed_elsewhere(record.as_ref(), &landing);
                return Err(MoveFencedError::with_detail(move_id, record, Some(detail)));
            }
            if matches!(
                record,
                Some(MoveRecord::Status(MoveStatus::AcceptedRevoked))
            ) {
                return Ok(());
            }
            if matches!(record, Some(MoveRecord::Status(MoveStatus::Accepted))) && !revoked {
                return Ok(());
            }
            file.write(
                move_id,
                &MoveRecord::Status(if revoked {
                    MoveStatus::AcceptedRevoked
                } else {
                    MoveStatus::Accepted
                }),
                &settled_at(&landing, target),
            )?;
            Ok(())
        })
        .await
    }

    pub async fn mark_revoked(&self, move_id: &str) -> Result<(), MoveLedgerError> {
        self.locked(move_id, || {
            let stamp_fn = || self.stamp();
            let file = self.record_file(&stamp_fn);
            let record = file.read(move_id)?;
            if record.as_ref().is_some_and(|r| {
                !matches!(
                    r,
                    MoveRecord::Status(
                        MoveStatus::Receiving | MoveStatus::Accepted | MoveStatus::AcceptedRevoked
                    )
                )
            }) {
                return Ok(());
            }
            if matches!(
                record,
                Some(MoveRecord::Status(MoveStatus::AcceptedRevoked))
            ) {
                return Ok(());
            }
            let landing = file.landing(move_id)?;
            let next_landing = match landing.target.as_deref() {
                Some(t) => settled_at(&landing, t),
                None => Landing::default(),
            };
            file.write(
                move_id,
                &MoveRecord::Status(MoveStatus::AcceptedRevoked),
                &next_landing,
            )?;
            Ok(())
        })
        .await
    }

    pub async fn fail_unless_landed(
        &self,
        move_id: &str,
        reason: FailReason,
        landed: impl Future<Output = bool>,
    ) -> Result<Option<MoveRecord>, MoveLedgerError> {
        self.locked_async(move_id, async {
            if landed.await {
                return self.current(move_id);
            }
            self.settle_locked(
                move_id,
                MoveRecord::Status(MoveStatus::Failed { reason }),
                &[],
            )
            .map(Some)
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
        self.locked(move_id, || self.settle_locked(move_id, next, keeps))
            .await
    }

    fn settle_locked(
        &self,
        move_id: &str,
        next: MoveRecord,
        keeps: &[&str],
    ) -> Result<MoveRecord, MoveLedgerError> {
        if let Some(record) = self.current(move_id)?
            && (record.is_final()
                || keeps.contains(&record.state())
                || self.landing(move_id)?.committing)
        {
            return Ok(record);
        }
        self.write(move_id, &next)?;
        Ok(next)
    }

    /// The record, with a ready move whose window has passed declined as expired first (on disk, under the lock).
    fn current(&self, move_id: &str) -> Result<Option<MoveRecord>, MoveLedgerError> {
        let record = self.read(move_id)?;
        let Some(MoveRecord::Ready { expires_at }) = &record else {
            return Ok(record);
        };
        let expires_ms = chrono::DateTime::parse_from_rfc3339(expires_at)
            .map_or(i64::MIN, |at| at.timestamp_millis());
        if (self.clock.now)() < expires_ms {
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
        let stamp_fn = || self.stamp();
        self.record_file(&stamp_fn).read(move_id)
    }

    fn landing(&self, move_id: &str) -> Result<Landing, MoveLedgerError> {
        let stamp_fn = || self.stamp();
        self.record_file(&stamp_fn).landing(move_id)
    }

    fn write(&self, move_id: &str, record: &MoveRecord) -> Result<(), MoveLedgerError> {
        self.write_landing(move_id, record, &Landing::default())
    }

    fn write_landing(
        &self,
        move_id: &str,
        record: &MoveRecord,
        landing: &Landing,
    ) -> Result<(), MoveLedgerError> {
        let stamp_fn = || self.stamp();
        self.record_file(&stamp_fn)
            .write(move_id, record, landing)
            .map(|_| ())
    }

    async fn locked<T>(
        &self,
        move_id: &str,
        work: impl FnOnce() -> Result<T, MoveLedgerError>,
    ) -> Result<T, MoveLedgerError> {
        self.locked_async(move_id, async { work() }).await
    }

    async fn locked_async<T>(
        &self,
        move_id: &str,
        work: impl Future<Output = Result<T, MoveLedgerError>>,
    ) -> Result<T, MoveLedgerError> {
        let file = self.path(move_id)?;
        let key = file.to_string_lossy().into_owned();
        serialize_by_key(&key, async {
            tokio::fs::create_dir_all(&self.dir).await?;
            let _guard = self
                .locks
                .hold(&file)
                .await
                .map_err(MoveLedgerError::Failed)?;
            self.locks
                .assert_held(&file)
                .map_err(MoveLedgerError::Failed)?;
            work.await
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
            ledger
                .begin_receive("m1", "/target")
                .await
                .expect("receive");
            assert_eq!(
                ledger.prepare("m1").await.expect("receiving stays"),
                MoveRecord::Status(MoveStatus::Receiving)
            );
            let unknown = ledger
                .begin_receive("m2", "/target")
                .await
                .expect_err("no ready");
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
            ledger
                .begin_receive("m1", "/target")
                .await
                .expect("receive");
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
            ledger
                .begin_receive("m1", "/target")
                .await
                .expect("receive");
            assert_eq!(
                ledger
                    .commit("m1", "/target", async { Ok(7) }, async { Ok(()) })
                    .await
                    .expect("commit"),
                7
            );
            assert_eq!(
                ledger.answer_status("m1", false).await.expect("answer"),
                MoveStatus::Accepted
            );
            ledger
                .decline("m4", DeclineReason::Cancelled)
                .await
                .expect("decline");
            assert!(matches!(
                ledger
                    .commit("m4", "/target", async { Ok(()) }, async { Ok(()) })
                    .await,
                Err(MoveLedgerError::Fenced(_))
            ));
        });
    }
}
