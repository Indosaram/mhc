use super::{branch_entries, entries, session_row, storage::SqliteStorage, values};
use maho_agent::harness::{context::Context, session::*};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;
fn error(error: impl std::fmt::Display) -> SessionError {
    SessionError::io(error.to_string())
}

pub const SQLITE_STORAGE_VERSION: u32 = 1;
pub const SQLITE_SESSION_EXTENSION: &str = ".sqlite";
pub struct SqliteSessionRepo {
    directory: PathBuf,
    database_path: Option<PathBuf>,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    sessions: Arc<Mutex<BTreeMap<String, Arc<StorageBackedSession>>>>,
    storages: Arc<Mutex<BTreeMap<String,Arc<SqliteStorage>>>>,
    closed: AtomicBool,
}
impl SqliteSessionRepo {
    pub fn new(
        directory: PathBuf,
        database_path: Option<PathBuf>,
        now: Arc<dyn Fn() -> i64 + Send + Sync>,
    ) -> Self {
        Self {
            directory,
            database_path,
            now,
            sessions: Arc::new(Mutex::new(BTreeMap::new())),
            storages: Arc::new(Mutex::new(BTreeMap::new())),
            closed: AtomicBool::new(false),
        }
    }
    fn path(&self, id: &str) -> PathBuf {
        if let Some(path) = &self.database_path {
            return path.clone();
        }
        let name = if !id.is_empty()
            && id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
        {
            id.to_owned()
        } else {
            const ALPHABET: &[u8] =
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
            let bytes = id
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>();
            let mut output = String::from("~");
            for chunk in bytes.chunks(3) {
                let n = (u32::from(chunk[0]) << 16)
                    | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
                    | u32::from(*chunk.get(2).unwrap_or(&0));
                output.push(char::from(ALPHABET[((n >> 18) & 63) as usize]));
                output.push(char::from(ALPHABET[((n >> 12) & 63) as usize]));
                if chunk.len() > 1 {
                    output.push(char::from(ALPHABET[((n >> 6) & 63) as usize]));
                }
                if chunk.len() > 2 {
                    output.push(char::from(ALPHABET[(n & 63) as usize]));
                }
            }
            output
        };
        self.directory
            .join(format!("{name}{SQLITE_SESSION_EXTENSION}"))
    }
    fn assert_open(&self) -> Result<(), SessionError> {
        if self.closed.load(Ordering::SeqCst) {
            Err(SessionError::new(
                SessionErrorKind::Closed,
                "SqliteSessionRepo is closed",
            ))
        } else {
            Ok(())
        }
    }
    fn session(
        &self,
        metadata: SessionMetadata,
        db: rusqlite::Connection,
        sessions: &mut BTreeMap<String, Arc<StorageBackedSession>>,
    ) -> Box<dyn Session> {
        let id = metadata.id.clone();
        let now = self.now.clone();
        let storage = Arc::new(SqliteStorage::new(
            db,
            id.clone(),
            Box::new(move || (now)()),
        ));
        let tracking = Arc::downgrade(&self.sessions);
        self.storages.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(id.clone(),storage.clone());
        let storage_tracking=Arc::downgrade(&self.storages);
        let session = Arc::new(StorageBackedSession::new(
            metadata,
            storage,
            StorageBackedSessionOptions {
                on_close: Some(Arc::new(move || {
                    if let Some(tracking) = tracking.upgrade() {
                        tracking
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .remove(&id);
                    }
                    if let Some(tracking)=storage_tracking.upgrade() {tracking.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&id);}
                })),
                ..Default::default()
            },
        ));
        session.attach();
        sessions.insert(session.metadata().id.clone(), session.clone());
        Box::new(session)
    }
    fn writable(path: &Path, create: bool) -> Result<rusqlite::Connection, SessionError> {
        let db = if create {
            super::open(path)
        } else {
            super::open_existing(path)
        }
        .map_err(error)?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")
            .map_err(error)?;
        Ok(db)
    }
    pub async fn close(&self, context: &Context) {
        self.closed.store(true, Ordering::SeqCst);
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for session in sessions {
            session.close(context).await;
        }
    }
}
impl SessionRepo for SqliteSessionRepo {
    fn create<'a>(
        &'a self,
        options: SessionCreateOptions,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, SessionError>> {
        Box::pin(async move {
            self.assert_open()?;
            let mut sessions = self.sessions.lock().map_err(error)?;
            let created_at = (self.now)();
            let id = options
                .id
                .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
            if sessions.contains_key(&id) {
                return Err(session_invariant_error(format!(
                    "Session is already open: {id}"
                )));
            }
            let path = self.path(&id);
            std::fs::create_dir_all(
                path.parent()
                    .ok_or_else(|| error("Missing database parent"))?,
            )
            .map_err(error)?;
            let reserved = self.database_path.is_none();
            if reserved {
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                    .map_err(error)?;
            }
            let initialize = || -> Result<_, SessionError> {
                let mut db = Self::writable(&path, true)?;
                super::apply_initial_schema(&db).map_err(error)?;
                let metadata = SessionMetadata {
                    id: id.clone(),
                    created_at,
                    storage_version: SQLITE_STORAGE_VERSION,
                    parent_session_id: options.parent_session_id,
                    cwd: None,
                    legacy_parent_session_path: None,
                };
                let tx = db
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                    .map_err(error)?;
                session_row::insert_session(&tx, &metadata, 1).map_err(error)?;
                values::set_scalar(&tx, &id, &branch_tip("main"), 0, &serde_json::Value::Null)
                    .map_err(error)?;
                tx.commit().map_err(error)?;
                Ok((metadata, db))
            };
            match initialize() {
                Ok((metadata, db)) => Ok(self.session(metadata, db, &mut sessions)),
                Err(failure) => {
                    if reserved {
                        std::fs::remove_file(&path).map_err(error)?;
                    }
                    Err(failure)
                }
            }
        })
    }
    fn open<'a>(
        &'a self,
        metadata: SessionMetadata,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, SessionError>> {
        Box::pin(async move {
            self.assert_open()?;
            let mut sessions = self.sessions.lock().map_err(error)?;
            if sessions.contains_key(&metadata.id) {
                return Err(session_invariant_error(format!(
                    "Session is already open: {}",
                    metadata.id
                )));
            }
            let db = Self::writable(&self.path(&metadata.id), false)?;
            let metadata = session_row::metadata_from_row(
                session_row::read_session(&db, &metadata.id).map_err(error)?,
                SQLITE_STORAGE_VERSION,
            )
            .map_err(error)?;
            Ok(self.session(metadata, db, &mut sessions))
        })
    }
    fn list<'a>(
        &'a self,
        _options: Option<serde_json::Value>,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<Vec<SessionMetadata>, SessionError>> {
        Box::pin(async move {
            self.assert_open()?;
            let paths = if let Some(path) = &self.database_path {
                vec![path.clone()]
            } else {
                match std::fs::read_dir(&self.directory) {
                    Ok(entries) => entries
                        .map(|entry| entry.map(|e| e.path()))
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(error)?
                        .into_iter()
                        .filter(|p| p.extension().is_some_and(|e| e == "sqlite"))
                        .collect(),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                    Err(e) => return Err(error(e)),
                }
            };
            let mut metadata = Vec::new();
            for path in paths {
                let discovered=||->Result<Vec<SessionMetadata>,SessionError> {
                    let db=super::open_read_only(&path).map_err(error)?;
                    db.execute_batch("PRAGMA busy_timeout=5000;").map_err(error)?;
                    session_row::read_all_sessions(&db).map_err(error)?.into_iter().map(|row|session_row::metadata_from_row(row,SQLITE_STORAGE_VERSION).map_err(error)).collect()
                };
                if let Ok(sessions)=discovered() {
                    metadata.extend(sessions);
                }
            }
            metadata.sort_by_key(|session|std::cmp::Reverse(session.created_at));
            Ok(metadata)
        })
    }
    fn delete<'a>(
        &'a self,
        metadata: SessionMetadata,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<(), SessionError>> {
        Box::pin(async move {
            self.assert_open()?;
            let sessions = self.sessions.lock().map_err(error)?;
            if sessions.contains_key(&metadata.id) {
                return Err(session_invariant_error(format!(
                    "Session is already open: {}",
                    metadata.id
                )));
            }
            let path = self.path(&metadata.id);
            let mut db = Self::writable(&path, false)?;
            session_row::metadata_from_row(
                session_row::read_session(&db, &metadata.id).map_err(error)?,
                SQLITE_STORAGE_VERSION,
            )
            .map_err(error)?;
            if self.database_path.is_some() {
                let tx = db
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                    .map_err(error)?;
                session_row::delete_session_rows(&tx, &metadata.id).map_err(error)?;
                tx.commit().map_err(error)?;
            } else {
                drop(db);
                std::fs::remove_file(&path).map_err(error)?;
                for suffix in ["-wal", "-shm"] {
                    match std::fs::remove_file(format!("{}{suffix}", path.display())) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => return Err(error(e)),
                    }
                }
            }
            Ok(())
        })
    }
    fn fork<'a>(
        &'a self,
        source: SessionMetadata,
        options: ForkOptions,
        _context: &'a Context,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, SessionError>> {
        Box::pin(async move {
            self.assert_open()?;
            let active=self.storages.lock().map_err(error)?.get(&source.id).cloned();
            let snapshot = if let Some(storage)=active {
                create_fork_snapshot(storage.snapshot(&options)?,&options)?
            } else {
                let mut db = super::open_read_only(&self.path(&source.id)).map_err(error)?;
                db.execute_batch("PRAGMA busy_timeout=5000;")
                    .map_err(error)?;
                let tx = db.transaction().map_err(error)?;
                let snapshot = create_fork_snapshot(
                    ForkSourceSnapshot {
                        entries: entries::scan_entries(&tx, &source.id, &EntryScan::default())
                            .map_err(error)?,
                        scalar_values: values::read_all_scalars(&tx, &source.id).map_err(error)?,
                        entries_complete: Some(true),
                    },
                    &options,
                )?;
                tx.commit().map_err(error)?;
                snapshot
            };
            let mut sessions = self.sessions.lock().map_err(error)?;
            let id = options.id().map(str::to_owned).unwrap_or_else(||uuid::Uuid::now_v7().to_string());
            if sessions.contains_key(&id) {return Err(session_invariant_error(format!("Session is already open: {id}")));}
            let path = self.path(&id);
            std::fs::create_dir_all(path.parent().ok_or_else(||error("Missing database parent"))?).map_err(error)?;
            let reserved = self.database_path.is_none();
            if reserved {std::fs::OpenOptions::new().write(true).create_new(true).open(&path).map_err(error)?;}
            let initialize = || -> Result<_,SessionError> {
                let mut db = Self::writable(&path, true)?;
                super::apply_initial_schema(&db).map_err(error)?;
                let metadata = SessionMetadata {id:id.clone(),created_at:(self.now)(),storage_version:SQLITE_STORAGE_VERSION,parent_session_id:Some(source.id),cwd:None,legacy_parent_session_path:None};
                let tx = db
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                    .map_err(error)?;
                session_row::insert_session(&tx,&metadata,snapshot.next_seq).map_err(error)?;
                let mut copied = snapshot.entries.into_values().collect::<Vec<_>>();
                copied.sort_by_key(|e| e.seq);
                let messages = copied
                    .iter()
                    .filter(|e| e.entry_type() == EntryType::Message)
                    .count();
                for entry in copied {
                    entries::insert_entry(&tx, &metadata.id, &entry).map_err(error)?;
                    branch_entries::append_to_branch_index(&tx, &metadata.id, &entry)
                        .map_err(error)?;
                }
                for stored in snapshot.scalar_values {
                    values::set_scalar(
                        &tx,
                        &metadata.id,
                        &stored.address,
                        stored.seq,
                        &stored.value,
                    )
                    .map_err(error)?;
                }
                tx.execute(
                    "UPDATE sessions SET next_seq=?1,message_count=?2 WHERE id=?3",
                    rusqlite::params![
                        snapshot.next_seq,
                        i64::try_from(messages).map_err(error)?,
                        metadata.id
                    ],
                )
                .map_err(error)?;
                tx.commit().map_err(error)?;
                Ok((metadata,db))
            };
            match initialize() {
                Ok((metadata,db))=>Ok(self.session(metadata,db,&mut sessions)),
                Err(failure)=>{
                    if reserved {
                        for suffix in ["", "-wal", "-shm"] {
                            match std::fs::remove_file(format!("{}{suffix}",path.display())) {
                                Ok(())=>{},
                                Err(cleanup) if cleanup.kind() == std::io::ErrorKind::NotFound=>{},
                                Err(cleanup)=>return Err(error(format!("{failure}; failed fork cleanup: {cleanup}"))),
                            }
                        }
                    }
                    Err(failure)
                },
            }
        })
    }
}
