//! Port of senpi packages/coding-agent/src/core/session-move/move-record-file.ts.
//!
//! The file a move's record is kept in, one per move in the receiving device's ledger folder:
//! where it is, how a record and its landing are read back from it, how it is replaced whole,
//! and the two checks a commit makes against it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::bundle::parse_move_id;
use super::move_landing::{Landing, is_commit_stamp, landed_elsewhere, lands_at, parse_landing};
use super::move_ledger::{MoveFencedError, MoveLedgerError, MoveRecord, parse_record};
use super::move_ledger_lock::MoveRecordLocks;

pub type StampFn<'a> = &'a (dyn Fn() -> BTreeMap<String, Value> + Send + Sync);

pub struct MoveRecordFile<'a> {
    pub dir: &'a Path,
    locks: &'a MoveRecordLocks,
    stamp: Option<StampFn<'a>>,
}

impl<'a> MoveRecordFile<'a> {
    pub fn new(dir: &'a Path, locks: &'a MoveRecordLocks, stamp: Option<StampFn<'a>>) -> Self {
        Self { dir, locks, stamp }
    }

    pub fn path(&self, move_id: &str) -> Result<PathBuf, MoveLedgerError> {
        let id = parse_move_id(&Value::String(move_id.to_owned()))
            .map_err(|error| MoveLedgerError::Failed(error.to_string()))?;
        Ok(self.dir.join(format!("{id}.json")))
    }

    pub fn raw(&self, move_id: &str) -> Result<Option<Value>, MoveLedgerError> {
        let path = self.path(move_id)?;
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|error| MoveLedgerError::Failed(error.to_string()))
    }

    pub fn read(&self, move_id: &str) -> Result<Option<MoveRecord>, MoveLedgerError> {
        self.record_in(move_id, self.raw(move_id)?.as_ref())
    }

    fn record_in(
        &self,
        move_id: &str,
        value: Option<&Value>,
    ) -> Result<Option<MoveRecord>, MoveLedgerError> {
        let Some(value) = value else {
            return Ok(None);
        };
        parse_record(value).map(Some).ok_or_else(|| {
            let path = self
                .path(move_id)
                .unwrap_or_else(|_| PathBuf::from(move_id));
            MoveLedgerError::Failed(format!(
                "Move record {} is not a move record",
                path.display()
            ))
        })
    }

    pub fn landing(&self, move_id: &str) -> Result<Landing, MoveLedgerError> {
        Ok(self
            .raw(move_id)?
            .as_ref()
            .map(parse_landing)
            .unwrap_or_default())
    }

    pub fn write(
        &self,
        move_id: &str,
        record: &MoveRecord,
        landing: &Landing,
    ) -> Result<String, MoveLedgerError> {
        let file = self.path(move_id)?;
        let temp = PathBuf::from(format!("{}.{}.tmp", file.display(), std::process::id()));
        let at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        self.locks
            .assert_held(&file)
            .map_err(MoveLedgerError::Failed)?;
        let mut obj = match record.to_json() {
            Value::Object(map) => map,
            _ => Map::new(),
        };
        if let Some(target) = &landing.target {
            obj.insert("target".to_owned(), json!(target));
        }
        if let Some(cwd) = &landing.cwd {
            obj.insert("cwd".to_owned(), json!(cwd));
        }
        if landing.committing {
            obj.insert("committing".to_owned(), json!(true));
        }
        obj.insert("at".to_owned(), json!(at));
        if let Some(stamp) = self.stamp {
            for (k, v) in stamp() {
                obj.insert(k, v);
            }
        }
        std::fs::write(&temp, format!("{}\n", Value::Object(obj)))?;
        self.locks
            .assert_held(&file)
            .map_err(MoveLedgerError::Failed)?;
        std::fs::rename(&temp, &file)?;
        Ok(at)
    }

    pub fn assert_lands_at(&self, move_id: &str, target: &str) -> Result<Landing, MoveLedgerError> {
        let stored = self.raw(move_id)?;
        let record = self.record_in(move_id, stored.as_ref())?;
        let landing = stored.as_ref().map(parse_landing).unwrap_or_default();
        if !lands_at(record.as_ref(), &landing, target) {
            let detail = landed_elsewhere(record.as_ref(), &landing);
            return Err(MoveFencedError::with_detail(move_id, record, Some(detail)));
        }
        Ok(landing)
    }

    pub fn assert_stamp_stands(&self, move_id: &str, stamp: &str) -> Result<(), MoveLedgerError> {
        let file = self.path(move_id)?;
        self.locks
            .assert_held(&file)
            .map_err(MoveLedgerError::Failed)?;
        let stored = self.raw(move_id)?;
        if stored.as_ref().is_some_and(|v| is_commit_stamp(v, stamp)) {
            return Ok(());
        }
        let record = stored.as_ref().and_then(parse_record);
        let replaced = if matches!(
            record,
            Some(MoveRecord::Status(
                super::move_ledger::MoveStatus::Receiving
            ))
        ) {
            Some("another import's commit took over its record".to_owned())
        } else {
            None
        };
        Err(MoveFencedError::with_detail(move_id, record, replaced))
    }
}
