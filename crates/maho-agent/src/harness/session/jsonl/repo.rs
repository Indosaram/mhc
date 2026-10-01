//! Port of senpi packages/agent/src/harness/session/jsonl/repo.ts.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::harness::context::{BACKGROUND_CONTEXT, Context};
use crate::harness::session::session::{
    SessionError, SessionErrorKind, StorageBackedSession, StorageBackedSessionOptions, session_invariant_error,
};
use crate::harness::session::types::{ForkOptions, Session};
use crate::harness::types::{FileInfo, FileSystem};

use super::codec::parse_jsonl_session_header;
use super::fork::{JsonlForkInput, JsonlForkSourceMetadata, run_jsonl_fork};
use super::io::file_value;
use super::storage::JsonlStorage;
use super::types::{
    JSONL_STORAGE_VERSION, JsonlSessionCreateOptions, JsonlSessionListOptions, JsonlSessionMetadata,
    JsonlSessionRepoOptions, JsonlStorageHeader, JsonlStorageOptions,
};

fn metadata_from_header(header: &JsonlStorageHeader, path: &str, modified_at: i64) -> JsonlSessionMetadata {
    JsonlSessionMetadata {
        id: header.id.clone(),
        created_at: header.created_at,
        storage_version: header.storage_version,
        cwd: header.cwd.clone(),
        path: path.to_owned(),
        modified_at,
        parent_session_id: header.parent_session_id.clone(),
        legacy_parent_session_path: header.legacy_parent_session_path.clone(),
    }
}

fn session_directory_name(cwd: &str) -> String {
    let without_leading = cwd.trim_start_matches(['/', '\\']);
    let replaced: String = without_leading
        .chars()
        .map(|character| match character {
            '/' | '\\' | ':' => '-',
            other => other,
        })
        .collect();
    format!("--{replaced}--")
}

fn session_file_name(created_at: i64, id: &str) -> String {
    let timestamp = iso_like_timestamp(created_at).replace([':', '.'], "-");
    format!("{timestamp}_{}.jsonl", percent_encode(id))
}

fn iso_like_timestamp(milliseconds: i64) -> String {
    let seconds = milliseconds.div_euclid(1000);
    let millis = milliseconds.rem_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let time_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = time_of_day / 3600;
    let minute = (time_of_day % 3600) / 60;
    let second = time_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// `encodeURIComponent` for the characters a session id can carry.
fn percent_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        let unreserved = byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')');
        if unreserved {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

type OpenSessions = Arc<Mutex<HashMap<String, Arc<JsonlStorage>>>>;

/// File-backed format-4 session repository lifecycle.
pub struct JsonlSessionRepo {
    file_system: Arc<dyn FileSystem>,
    sessions_root_input: String,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    open_sessions: OpenSessions,
    pending_creates: Mutex<HashSet<String>>,
    closed: AtomicBool,
}

impl JsonlSessionRepo {
    pub fn new(options: JsonlSessionRepoOptions) -> Self {
        Self {
            file_system: options.file_system,
            sessions_root_input: options.sessions_root,
            now: options.now.unwrap_or_else(|| {
                Arc::new(|| {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|elapsed| elapsed.as_millis() as i64)
                        .unwrap_or_default()
                })
            }),
            open_sessions: Arc::new(Mutex::new(HashMap::new())),
            pending_creates: Mutex::new(HashSet::new()),
            closed: AtomicBool::new(false),
        }
    }

    fn assert_open(&self) -> Result<(), SessionError> {
        if self.closed.load(Ordering::SeqCst) {
            Err(SessionError::new(
                SessionErrorKind::Closed,
                "JsonlSessionRepo is closed",
            ))
        } else {
            Ok(())
        }
    }

    fn session_key(cwd: &str, id: &str) -> String {
        format!("{cwd}\u{0}{id}")
    }

    async fn root(&self, context: &Context) -> Result<String, SessionError> {
        file_value(
            self.file_system.absolute_path(&self.sessions_root_input, context).await,
            &format!("Failed to resolve sessions root {}", self.sessions_root_input),
        )
    }

    async fn session_directory(&self, cwd: &str, context: &Context) -> Result<String, SessionError> {
        let root = self.root(context).await?;
        file_value(
            self.file_system
                .join_path(vec![root, session_directory_name(cwd)], context)
                .await,
            &format!("Failed to resolve sessions directory for {cwd}"),
        )
    }

    async fn resolve_new_session_path(
        &self,
        cwd: &str,
        created_at: i64,
        id: &str,
        context: &Context,
    ) -> Result<String, SessionError> {
        let directory = self.session_directory(cwd, context).await?;
        self.assert_session_id_available(&directory, id, context).await?;
        file_value(
            self.file_system.create_dir(&directory, None, context).await,
            &format!("Failed to create sessions directory {directory}"),
        )?;
        file_value(
            self.file_system
                .join_path(vec![directory, session_file_name(created_at, id)], context)
                .await,
            &format!("Failed to resolve path for session {id}"),
        )
    }

    async fn assert_session_id_available(
        &self,
        directory: &str,
        id: &str,
        context: &Context,
    ) -> Result<(), SessionError> {
        if !file_value(
            self.file_system.exists(directory, context).await,
            &format!("Failed to check sessions directory {directory}"),
        )? {
            return Ok(());
        }
        let suffix = format!("_{}.jsonl", percent_encode(id));
        let id_exists = file_value(
            self.file_system.list_dir(directory, context).await,
            &format!("Failed to list sessions directory {directory}"),
        )?
        .iter()
        .any(|entry| entry.kind != crate::harness::types::FileKind::Directory && entry.name.ends_with(&suffix));
        if id_exists {
            return Err(session_invariant_error(format!("Session already exists: {id}")));
        }
        Ok(())
    }

    fn reserve(&self, key: &str, id: &str) -> Result<(), SessionError> {
        let mut pending = self.pending_creates.lock().expect("pending creates");
        if self.open_sessions.lock().expect("open sessions").contains_key(key) || pending.contains(key) {
            return Err(session_invariant_error(format!("Session already exists: {id}")));
        }
        pending.insert(key.to_owned());
        Ok(())
    }

    fn release(&self, key: &str) {
        self.pending_creates.lock().expect("pending creates").remove(key);
    }

    fn publish_open_session(
        &self,
        metadata: JsonlSessionMetadata,
        storage: Arc<JsonlStorage>,
    ) -> Result<Box<dyn Session>, SessionError> {
        let key = Self::session_key(&metadata.cwd, &metadata.id);
        if self.open_sessions.lock().expect("open sessions").contains_key(&key) {
            return Err(session_invariant_error(format!(
                "Session is already open: {}",
                metadata.id
            )));
        }
        let open_sessions = self.open_sessions.clone();
        let storage_for_close = storage.clone();
        let key_for_close = key.clone();
        let session = Arc::new(StorageBackedSession::new(
            metadata.to_session_metadata(),
            storage.clone(),
            StorageBackedSessionOptions {
                on_close: Some(Arc::new(move || {
                    let mut sessions = open_sessions.lock().expect("open sessions");
                    if sessions
                        .get(&key_for_close)
                        .is_some_and(|open| Arc::ptr_eq(open, &storage_for_close))
                    {
                        sessions.remove(&key_for_close);
                    }
                })),
                ..StorageBackedSessionOptions::default()
            },
        ));
        session.attach();
        self.open_sessions
            .lock()
            .expect("open sessions")
            .insert(key, storage);
        Ok(Box::new(session))
    }

    async fn load_storage(
        &self,
        metadata: &JsonlSessionMetadata,
        context: &Context,
    ) -> Result<JsonlStorage, SessionError> {
        if !file_value(
            self.file_system.exists(&metadata.path, context).await,
            &format!("Failed to check session {}", metadata.path),
        )? {
            return Err(session_invariant_error(format!(
                "Session file does not exist: {}",
                metadata.path
            )));
        }
        let storage = JsonlStorage::open(
            super::types::JsonlStorageOptions {
                file_system: self.file_system.clone(),
                path: metadata.path.clone(),
                now: Some(self.now.clone()),
            },
            context,
        )
        .await?;
        if storage.header().id != metadata.id || storage.header().cwd != metadata.cwd {
            return Err(session_invariant_error(format!(
                "Session identity does not match header: {}",
                metadata.id
            )));
        }
        if storage.header().storage_version != JSONL_STORAGE_VERSION {
            return Err(session_invariant_error(format!(
                "Session {} uses unsupported storage version {}",
                metadata.id,
                storage.header().storage_version
            )));
        }
        Ok(storage)
    }

    async fn list_directory(
        &self,
        directory: &str,
        cwd: Option<&str>,
        context: &Context,
    ) -> Result<Vec<JsonlSessionMetadata>, SessionError> {
        if !file_value(
            self.file_system.exists(directory, context).await,
            &format!("Failed to check sessions directory {directory}"),
        )? {
            return Ok(Vec::new());
        }
        let files = file_value(
            self.file_system.list_dir(directory, context).await,
            &format!("Failed to list sessions directory {directory}"),
        )?;
        let mut metadata: Vec<JsonlSessionMetadata> = Vec::new();
        for file in files {
            if file.kind == crate::harness::types::FileKind::Directory || !file.name.ends_with(".jsonl") {
                continue;
            }
            let Some(discovered) = self.read_session_metadata(&file, context).await? else {
                continue;
            };
            if cwd.is_none() || Some(discovered.cwd.as_str()) == cwd {
                metadata.push(discovered);
            }
        }
        Ok(metadata)
    }

    async fn read_session_metadata(
        &self,
        file: &FileInfo,
        context: &Context,
    ) -> Result<Option<JsonlSessionMetadata>, SessionError> {
        let lines = file_value(
            self.file_system
                .read_text_lines(&file.path, Some(1), context)
                .await,
            &format!("Failed to read session header {}", file.path),
        )?;
        let Some(line) = lines.first() else {
            return Ok(None);
        };
        let Ok(parsed) = parse_jsonl_session_header(line) else {
            return Ok(None);
        };
        match parsed {
            super::codec::JsonlParsedSessionHeader::V4(header) => {
                Ok(Some(metadata_from_header(&header, &file.path, file.mtime_ms as i64)))
            }
            super::codec::JsonlParsedSessionHeader::V3Legacy(_) => Ok(None),
        }
    }
}

impl JsonlSessionRepo {
    /// `create(options: JsonlSessionCreateOptions)`: cwd-scoped format-4 session creation.
    pub async fn create(
        &self,
        options: JsonlSessionCreateOptions,
        context: &Context,
    ) -> Result<Box<dyn Session>, SessionError> {
        self.assert_open()?;
        let created_at = (self.now)();
        let id = options
            .id
            .clone()
            .unwrap_or_else(|| maho_ai::utils::uuid::uuidv7(Some(created_at)).unwrap_or_default());
        let cwd = file_value(
            self.file_system.absolute_path(&options.cwd, context).await,
            &format!("Failed to resolve session cwd {}", options.cwd),
        )?;
        let key = Self::session_key(&cwd, &id);
        self.reserve(&key, &id)?;
        let result = async {
            let path = self.resolve_new_session_path(&cwd, created_at, &id, context).await?;
            let mut header = JsonlStorageHeader::new(id.clone(), JSONL_STORAGE_VERSION, created_at, cwd.clone());
            header.parent_session_id = options.parent_session_id.clone();
            let storage = JsonlStorage::create(
                JsonlStorageOptions {
                    file_system: self.file_system.clone(),
                    path: path.clone(),
                    now: Some(self.now.clone()),
                },
                header.clone(),
                Vec::new(),
                context,
            )
            .await?;
            let info = file_value(
                self.file_system.file_info(&path, context).await,
                &format!("Failed to read session {path}"),
            )?;
            Ok::<_, SessionError>((metadata_from_header(&header, &path, info.mtime_ms as i64), Arc::new(storage)))
        }
        .await;
        self.release(&key);
        let (metadata, storage) = result?;
        self.publish_open_session(metadata, storage)
    }

    /// `open(metadata: JsonlSessionMetadata)`.
    pub async fn open(
        &self,
        metadata: JsonlSessionMetadata,
        context: &Context,
    ) -> Result<Box<dyn Session>, SessionError> {
        self.assert_open()?;
        let key = Self::session_key(&metadata.cwd, &metadata.id);
        if self.open_sessions.lock().expect("open sessions").contains_key(&key) {
            return Err(session_invariant_error(format!(
                "Session is already open: {}",
                metadata.id
            )));
        }
        let storage = self.load_storage(&metadata, context).await?;
        self.publish_open_session(metadata, Arc::new(storage))
    }

    /// `list(options: JsonlSessionListOptions | undefined)`.
    pub async fn list(
        &self,
        options: Option<JsonlSessionListOptions>,
        context: &Context,
    ) -> Result<Vec<JsonlSessionMetadata>, SessionError> {
        self.assert_open()?;
        let options = options.unwrap_or_default();
        let cwd = match &options.cwd {
            Some(cwd) => Some(file_value(
                self.file_system.absolute_path(cwd, context).await,
                &format!("Failed to resolve session cwd {cwd}"),
            )?),
            None => None,
        };
        let root = self.root(context).await?;
        if !file_value(
            self.file_system.exists(&root, context).await,
            &format!("Failed to check sessions root {root}"),
        )? {
            return Ok(Vec::new());
        }
        let directories = match &cwd {
            None => file_value(
                self.file_system.list_dir(&root, context).await,
                &format!("Failed to list sessions root {root}"),
            )?
            .into_iter()
            .filter(|entry| entry.kind == crate::harness::types::FileKind::Directory)
            .map(|entry| entry.path)
            .collect::<Vec<String>>(),
            Some(cwd) => vec![self.session_directory(cwd, context).await?],
        };
        let mut metadata: Vec<JsonlSessionMetadata> = Vec::new();
        for directory in directories {
            metadata.extend(self.list_directory(&directory, cwd.as_deref(), context).await?);
        }
        metadata.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| left.id.cmp(&right.id))
                .then_with(|| left.cwd.cmp(&right.cwd))
        });
        Ok(metadata)
    }

    /// `delete(metadata: JsonlSessionMetadata)`.
    pub async fn delete(&self, metadata: JsonlSessionMetadata, context: &Context) -> Result<(), SessionError> {
        self.assert_open()?;
        let key = Self::session_key(&metadata.cwd, &metadata.id);
        if self.open_sessions.lock().expect("open sessions").contains_key(&key) {
            return Err(session_invariant_error(format!("Session is open: {}", metadata.id)));
        }
        if !file_value(
            self.file_system.exists(&metadata.path, context).await,
            &format!("Failed to check session {}", metadata.path),
        )? {
            return Err(session_invariant_error(format!(
                "Session file does not exist: {}",
                metadata.path
            )));
        }
        file_value(
            self.file_system.remove(&metadata.path, None, None, context).await,
            &format!("Failed to delete session {}", metadata.path),
        )?;
        Ok(())
    }

    /// `fork(source: JsonlSessionMetadata, options: ForkOptions)`.
    pub async fn fork(
        &self,
        source: JsonlSessionMetadata,
        options: ForkOptions,
        context: &Context,
    ) -> Result<Box<dyn Session>, SessionError> {
        self.assert_open()?;
        let created_at = (self.now)();
        let cwd = source.cwd.clone();
        let id = options
            .id()
            .map(str::to_owned)
            .unwrap_or_else(|| maho_ai::utils::uuid::uuidv7(Some(created_at)).unwrap_or_default());
        let key = Self::session_key(&cwd, &id);
        self.reserve(&key, &id)?;
        let result = async {
            let input = self.resolve_fork_input(&source).await?;
            let path = self.resolve_new_session_path(&cwd, created_at, &id, context).await?;
            let mut header = JsonlStorageHeader::new(id.clone(), JSONL_STORAGE_VERSION, created_at, cwd.clone());
            header.parent_session_id = Some(source.id.clone());
            run_jsonl_fork(&input, &self.file_system, &path, &header, &options, context).await?;
            let storage = JsonlStorage::open(
                JsonlStorageOptions {
                    file_system: self.file_system.clone(),
                    path: path.clone(),
                    now: Some(self.now.clone()),
                },
                context,
            )
            .await?;
            let info = file_value(
                self.file_system.file_info(&path, context).await,
                &format!("Failed to read session {path}"),
            )?;
            Ok::<_, SessionError>((metadata_from_header(&header, &path, info.mtime_ms as i64), Arc::new(storage)))
        }
        .await;
        self.release(&key);
        let (metadata, storage) = result?;
        self.publish_open_session(metadata, storage)
    }

    pub async fn close(&self, _context: &Context) {
        self.closed.store(true, Ordering::SeqCst);
    }

}

impl JsonlSessionRepo {
    async fn resolve_fork_input(
        &self,
        source: &JsonlSessionMetadata,
    ) -> Result<JsonlForkInput, SessionError> {
        let metadata = JsonlForkSourceMetadata {
            id: source.id.clone(),
            cwd: source.cwd.clone(),
            path: source.path.clone(),
        };
        let open_storage = self
            .open_sessions
            .lock()
            .expect("open sessions")
            .get(&Self::session_key(&source.cwd, &source.id))
            .cloned();
        match open_storage {
            Some(storage) => {
                if storage.is_legacy_v3() {
                    return Err(session_invariant_error(
                        "Cannot fork an open legacy v3 JSONL session; commit a non-empty transaction to upgrade it to format 4 first",
                    ));
                }
                let next_seq = storage.capture_fork_next_seq(&BACKGROUND_CONTEXT).await?;
                Ok(JsonlForkInput::Open { metadata, next_seq })
            }
            None => Ok(JsonlForkInput::Closed { metadata }),
        }
    }
}

pub type JsonlSessionRepoHandle = Arc<JsonlSessionRepo>;
