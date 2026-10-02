use super::{entries, session_row, values};
use maho_agent::harness::{context::Context, session::*, utils::usage::add_usage};
use rusqlite::{Connection, params};
use std::{
    collections::BTreeMap,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

pub struct SqliteStorage {
    db: Mutex<Connection>,
    session_id: String,
    closed: AtomicBool,
    now: Box<dyn Fn() -> i64 + Send + Sync>,
}
fn error(error: impl std::fmt::Display) -> SessionError {
    SessionError::io(error.to_string())
}
impl SqliteStorage {
    pub fn new(
        db: Connection,
        session_id: String,
        now: Box<dyn Fn() -> i64 + Send + Sync>,
    ) -> Self {
        Self {
            db: Mutex::new(db),
            session_id,
            closed: AtomicBool::new(false),
            now,
        }
    }
    fn db(&self) -> Result<std::sync::MutexGuard<'_, Connection>, SessionError> {
        let db = self.db.lock().map_err(error)?;
        if self.closed.load(Ordering::SeqCst) {
            return Err(SessionError::new(
                SessionErrorKind::Closed,
                "SqliteStorage is closed",
            ));
        }
        Ok(db)
    }
    fn stats(&self, db: &Connection) -> Result<SessionStats, SessionError> {
        let row = session_row::read_session(db, &self.session_id).map_err(error)?;
        Ok(SessionStats {
            message_count: usize::try_from(row.message_count).map_err(error)?,
            usage: serde_json::from_str(&row.usage_payload).map_err(error)?,
        })
    }
    pub fn snapshot(&self,options:&ForkOptions)->Result<ForkSourceSnapshot,SessionError> {
        let mut db=self.db()?;
        let tx=db.transaction().map_err(error)?;
        let scalar_values=values::read_all_scalars(&tx,&self.session_id).map_err(error)?;
        let entries=match options {
            ForkOptions::Tree {..}=>entries::scan_entries(&tx,&self.session_id,&EntryScan::default()).map_err(error)?,
            ForkOptions::Branch {branch,..}=>{
                let address=branch_tip(branch);
                let tip=scalar_values.iter().find(|stored|stored.address==address).ok_or_else(||session_invariant_error(format!("Unknown source branch: {branch}")))?;
                match &tip.value {
                    serde_json::Value::Null=>Vec::new(),
                    serde_json::Value::String(start)=>{
                        let mut query=StorageBranchScan::new(start);
                        query.order=Some(BranchOrder::OldestFirst);
                        super::branch_entries::scan_branch(&tx,&self.session_id,&query).map_err(error)?
                    },
                    _=>return Err(session_invariant_error("Invalid source branch tip")),
                }
            },
        };
        tx.commit().map_err(error)?;
        Ok(ForkSourceSnapshot {entries,scalar_values,entries_complete:Some(matches!(options,ForkOptions::Tree {..}))})
    }
    fn branch(
        &self,
        db: &Connection,
        query: &StorageBranchScan,
    ) -> Result<Vec<Entry>, SessionError> {
        super::branch_entries::scan_branch(db, &self.session_id, query).map_err(error)
    }
}
impl Storage for SqliteStorage {
    fn commit<'a>(
        &'a self,
        writes: Vec<Write>,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<CommitResult, SessionError>> {
        Box::pin(async move {
            let mut db = self.db()?;
            let transaction = db
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(error)?;
            let first_seq = session_row::read_session(&transaction, &self.session_id)
                .map_err(error)?
                .next_seq;
            let prepared = prepare_storage_commit(writes, first_seq, (self.now)());
            let mut stats = self.stats(&transaction)?;
            for write in &prepared.writes {
                match write {
                    CommittedWrite::Entry(write) => {
                        let entry = Entry {
                            id: write.id.clone(),
                            parent_id: write.parent_id.clone(),
                            seq: write.seq,
                            timestamp: write.timestamp,
                            kind: write.body.clone(),
                        };
                        entries::insert_entry(&transaction, &self.session_id, &entry)
                            .map_err(error)?;
                        super::branch_entries::append_to_branch_index(
                            &transaction,
                            &self.session_id,
                            &entry,
                        )
                        .map_err(error)?;
                        if entry.entry_type() == EntryType::Message {
                            stats.message_count += 1;
                        }
                    }
                    CommittedWrite::Usage(write) => {
                        let usage = UsageRow {
                            id: write.id.clone(),
                            seq: write.seq,
                            usage: write.usage,
                            entry_id: write.entry_id.clone(),
                            adjustment: write.adjustment,
                            details: write.details.clone(),
                        };
                        entries::insert_usage(&transaction, &self.session_id, &usage)
                            .map_err(error)?;
                        stats.usage = add_usage(&stats.usage, &usage.usage);
                    }
                    CommittedWrite::Value(write) => {
                        let address = Value {
                            namespace: write.namespace().into(),
                            key: write.key().into(),
                        };
                        if let Some(value) = write.value() {
                            values::set_scalar(
                                &transaction,
                                &self.session_id,
                                &address,
                                write.seq(),
                                value,
                            )
                            .map_err(error)?;
                        } else {
                            values::delete_scalar(&transaction, &self.session_id, &address)
                                .map_err(error)?;
                        }
                    }
                    CommittedWrite::List(write) => {
                        let address = ValueList {
                            namespace: write.namespace().into(),
                            key: write.key().into(),
                        };
                        match write {
                            CommittedListWrite::Append(write) => values::append_list(
                                &transaction,
                                &self.session_id,
                                &address,
                                write.seq,
                                &write.value,
                            )
                            .map_err(error)?,
                            CommittedListWrite::Delete(_) => {
                                values::delete_list(&transaction, &self.session_id, &address)
                                    .map_err(error)?
                            }
                        }
                    }
                }
            }
            let next_seq = first_seq + i64::try_from(prepared.writes.len()).map_err(error)?;
            transaction
                .execute(
                    "UPDATE sessions SET next_seq=?1,message_count=?2,usage_payload=?3 WHERE id=?4",
                    params![
                        next_seq,
                        i64::try_from(stats.message_count).map_err(error)?,
                        serde_json::to_string(&stats.usage).map_err(error)?,
                        self.session_id
                    ],
                )
                .map_err(error)?;
            transaction.commit().map_err(error)?;
            Ok(CommitResult {
                first_seq: prepared.result.first_seq,
                seqs: prepared.result.seqs,
                timestamp: prepared.result.timestamp,
                stats,
            })
        })
    }
    fn get_entries<'a>(
        &'a self,
        ids: Vec<String>,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<BTreeMap<String, Entry>, SessionError>> {
        Box::pin(async move {
            let db = self.db()?;
            Ok(entries::read_entries(&db, &self.session_id, &ids)
                .map_err(error)?
                .into_iter()
                .map(|e| (e.id.clone(), e))
                .collect())
        })
    }
    fn get_value<'a>(
        &'a self,
        address: &'a Value,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<Option<StoredValue>, SessionError>> {
        Box::pin(async move {
            values::read_scalar(&*self.db()?, &self.session_id, address).map_err(error)
        })
    }
    fn scan_values<'a>(
        &'a self,
        prefix: &'a Value,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<Vec<StoredValue>, SessionError>> {
        Box::pin(async move {
            values::scan_scalars(&*self.db()?, &self.session_id, prefix).map_err(error)
        })
    }
    fn read_list<'a>(
        &'a self,
        address: &'a ValueList,
        options: Option<ListReadOptions>,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<Vec<ListElement>, SessionError>> {
        Box::pin(async move {
            values::read_list(&*self.db()?, &self.session_id, address, options).map_err(error)
        })
    }
    fn scan_branch<'a>(
        &'a self,
        query: StorageBranchScan,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<Vec<Entry>, SessionError>> {
        Box::pin(async move { self.branch(&*self.db()?, &query) })
    }
    fn scan_branch_structure<'a>(
        &'a self,
        query: StorageBranchScan,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<Vec<EntryStructure>, SessionError>> {
        Box::pin(async move {
            super::branch_entries::scan_branch_structure(&*self.db()?,&self.session_id,&query).map_err(error)
        })
    }
    fn scan_entries<'a>(
        &'a self,
        query: EntryScan,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<Vec<Entry>, SessionError>> {
        Box::pin(async move {
            entries::scan_entries(&*self.db()?, &self.session_id, &query).map_err(error)
        })
    }
    fn scan_usage<'a>(
        &'a self,
        query: UsageScan,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<Vec<UsageRow>, SessionError>> {
        Box::pin(async move {
            entries::scan_usage(&*self.db()?, &self.session_id, &query).map_err(error)
        })
    }
    fn get_stats<'a>(
        &'a self,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<SessionStats, SessionError>> {
        Box::pin(async move { self.stats(&*self.db()?) })
    }
    fn close<'a>(&'a self, _context: &'a Context) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let _db = self
                .db
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.closed.store(true, Ordering::SeqCst);
        })
    }
}
