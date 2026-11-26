//! Port of senpi packages/coding-agent/src/core/session-store/durable-binding.ts.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use maho_agent::durable::store::{StoreWrite, SyncCommitStore};
use maho_agent::durable::store_admission::{AdmissionLine, create_admission_line};
use maho_agent::durable::{
    SessionHeader, StoredEntry, SyncSessionHandle, open_session_store, open_sync_session_store,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;

use crate::paths::{PathInputOptions, normalize_path};
use crate::session_file_identity::{FileIdentity, capture_file_identity};
use crate::session_import::jsonl_import::{
    JsonlFileStamp, JsonlImporter, JsonlImporterOptions, ProjectedTail, RealJsonlFiles,
    create_jsonl_importer, stamp_from_metadata,
};

#[async_trait::async_trait]
pub trait ProjectionFiles: Send + Sync {
    async fn replace(&self, path: &Path, contents: &str) -> std::io::Result<()>;
}

#[derive(Clone)]
pub struct DurableStoreBinding {
    pub store: Arc<dyn SyncCommitStore>,
    pub epoch: u64,
    pub files: Option<Arc<dyn ProjectionFiles>>,
}

pub trait ProjectionHooks: Send + Sync {
    fn before_write(&self, path: &str) -> Result<(), String>;
    fn after_replace(&self, path: &str, identity: Option<FileIdentity>);
    fn after_replace_with_contents(
        &self,
        path: &str,
        identity: Option<FileIdentity>,
        _contents: &str,
    ) {
        self.after_replace(path, identity);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectionWarning {
    pub path: String,
    pub code: String,
    pub message: String,
}

pub type ProjectionWarnCallback = Arc<dyn Fn(ProjectionWarning) + Send + Sync>;

/// The S07 importer's key rule: an entry id that starts with `~` gets one more.
pub fn store_key_of(entry_id: &str) -> String {
    if entry_id.starts_with('~') {
        format!("~{entry_id}")
    } else {
        entry_id.to_owned()
    }
}

const STALE_PREFIX: &str = "session-store/projection-stale/";

fn encode_uri_component(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        let unreserved = byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')'
            );
        if unreserved {
            out.push(*byte as char);
        } else {
            out.push_str(&format!("%{:02X}", byte));
        }
    }
    out
}

fn stale_key(path: &str) -> String {
    format!("{STALE_PREFIX}{}", encode_uri_component(path))
}

fn projection_image(header: &SessionHeader, entries: &[StoredEntry]) -> String {
    let header_line = Value::Object(header.clone()).to_string();
    let mut out = String::new();
    out.push_str(&header_line);
    out.push('\n');
    for entry in entries {
        out.push_str(&entry.line);
        out.push('\n');
    }
    out
}

async fn stamp_of(path: &Path) -> std::io::Result<Option<JsonlFileStamp>> {
    match tokio::fs::metadata(path).await {
        Ok(meta) => Ok(Some(stamp_from_metadata(&meta))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

async fn replace_atomically(path: &Path, contents: &str) -> std::io::Result<Option<FileIdentity>> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let base = path.file_name().map_or_else(
        || "session.jsonl".to_owned(),
        |n| n.to_string_lossy().into_owned(),
    );
    let temp = parent.join(format!(".{base}.{}.tmp", uuid::Uuid::new_v4()));
    let mut opts = tokio::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = tokio::fs::metadata(path)
            .await
            .map_or(0o600, |m| m.permissions().mode() & 0o777);
        opts.mode(mode);
    }
    let mut file = opts.open(&temp).await?;
    let path_str = path.to_string_lossy().into_owned();
    let write_res: std::io::Result<Option<FileIdentity>> = async {
        file.write_all(contents.as_bytes()).await?;
        file.sync_all().await?;
        let std_file = file
            .try_into_std()
            .map_err(|_| std::io::Error::other("pending tokio write on sync handle"))?;
        let identity = capture_file_identity(&std_file, &path_str);
        drop(std_file);
        Ok(identity)
    }
    .await;
    let identity = match write_res {
        Ok(id) => id,
        Err(err) => {
            let _ = tokio::fs::remove_file(&temp).await;
            return Err(err);
        }
    };
    if let Err(err) = tokio::fs::rename(&temp, path).await {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(err);
    }
    Ok(identity)
}

async fn replace_with(
    files: Option<&dyn ProjectionFiles>,
    path: &Path,
    contents: &str,
) -> std::io::Result<Option<FileIdentity>> {
    match files {
        None => replace_atomically(path, contents).await,
        Some(custom) => {
            custom.replace(path, contents).await?;
            Ok(None)
        }
    }
}

fn resolve_path(path: &str) -> String {
    normalize_path(path, &PathInputOptions::default())
}

struct BindingState {
    projected_size: u64,
    unrecorded_tails: Vec<ProjectedTail>,
    whole_unrecorded: bool,
}

#[derive(Default)]
struct PendingProjectionWork {
    count: AtomicUsize,
    changed: tokio::sync::Notify,
}

struct ProjectionWorkGuard(Arc<PendingProjectionWork>);

impl Drop for ProjectionWorkGuard {
    fn drop(&mut self) {
        if self.0.count.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.changed.notify_waiters();
        }
    }
}

impl PendingProjectionWork {
    fn track(self: &Arc<Self>) -> ProjectionWorkGuard {
        self.count.fetch_add(1, Ordering::SeqCst);
        ProjectionWorkGuard(Arc::clone(self))
    }
}

/// A `SessionManager`'s binding to the durable store (S20b).
pub struct DurableSessionBinding {
    stale: AtomicBool,
    generation: AtomicU64,
    state: Mutex<BindingState>,
    binding: DurableStoreBinding,
    handle: SyncSessionHandle,
    importer: Arc<JsonlImporter>,
    admission: AdmissionLine,
    pending_work: Arc<PendingProjectionWork>,
    runtime: tokio::runtime::Handle,
    warn: ProjectionWarnCallback,
    hooks: Arc<dyn ProjectionHooks>,
}

impl std::fmt::Debug for DurableSessionBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DurableSessionBinding")
            .field("stale", &self.stale.load(Ordering::SeqCst))
            .field("generation", &self.generation.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}

impl DurableSessionBinding {
    pub async fn open(
        binding: DurableStoreBinding,
        session_file: &str,
        session_id: &str,
        header_line: &str,
        warn: ProjectionWarnCallback,
        hooks: Arc<dyn ProjectionHooks>,
    ) -> Result<(Arc<Self>, bool), String> {
        let mut header: SessionHeader =
            serde_json::from_str(header_line).map_err(|e| e.to_string())?;
        if header.get("id").and_then(Value::as_str) != Some(session_id) {
            return Err("session header ID does not match the session ID".to_owned());
        }
        let path_str = resolve_path(session_file);
        let path = Path::new(&path_str);
        let admission = create_admission_line();
        let importer = create_jsonl_importer(JsonlImporterOptions {
            store: binding.store.clone(),
            epoch: binding.epoch,
            admission: admission.clone(),
            files: Some(Arc::new(RealJsonlFiles)),
            now: None,
            mtime_tick_ms: None,
        });

        let mut id = session_id.to_owned();
        let mut reprojected = false;
        if stamp_of(path).await.map_err(|e| e.to_string())?.is_some() {
            let mark = binding
                .store
                .get_document(&stale_key(&path_str))
                .await
                .map_err(|e| e.to_string())?;
            let marked_id = mark
                .as_ref()
                .and_then(Value::as_object)
                .and_then(|o| o.get("sessionId"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            if let Some(marked) = marked_id {
                let stored = open_session_store(binding.store.clone(), binding.epoch)
                    .load(&marked)
                    .await
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| {
                        format!("the store marks {path_str} stale but holds no session {marked}")
                    })?;
                if stored.header.get("id").and_then(Value::as_str) != Some(marked.as_str()) {
                    return Err("stored session header ID does not match the session ID".to_owned());
                }
                let contents = projection_image(&stored.header, &stored.entries);
                reproject_file(
                    &importer,
                    &admission,
                    &binding,
                    hooks.as_ref(),
                    &path_str,
                    &contents,
                )
                .await?;
                header = stored.header;
                id = marked;
                reprojected = true;
            } else {
                id = importer.import_file(path).await?.session_id;
                if id != session_id {
                    let stored = open_session_store(binding.store.clone(), binding.epoch)
                        .load(&id)
                        .await
                        .map_err(|e| e.to_string())?
                        .ok_or_else(|| {
                            format!("imported session {id} is missing from the store")
                        })?;
                    if stored.header.get("id").and_then(Value::as_str) != Some(id.as_str()) {
                        return Err(
                            "stored session header ID does not match the session ID".to_owned()
                        );
                    }
                    let contents = projection_image(&stored.header, &stored.entries);
                    reproject_file(
                        &importer,
                        &admission,
                        &binding,
                        hooks.as_ref(),
                        &path_str,
                        &contents,
                    )
                    .await?;
                    header = stored.header;
                    reprojected = true;
                }
            }
        }
        let handle = open_sync_session_store(binding.store.clone(), binding.epoch)
            .open(&id, header)
            .await
            .map_err(|e| e.to_string())?;
        if handle.header().get("id").and_then(Value::as_str) != Some(id.as_str()) {
            return Err("stored session header ID does not match the session ID".to_owned());
        }
        let size = stamp_of(path)
            .await
            .map_err(|e| e.to_string())?
            .map_or(0, |s| s.size);

        let session = Arc::new(Self {
            stale: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            state: Mutex::new(BindingState {
                projected_size: size,
                unrecorded_tails: Vec::new(),
                whole_unrecorded: false,
            }),
            binding,
            handle,
            importer,
            admission,
            pending_work: Arc::new(PendingProjectionWork::default()),
            runtime: tokio::runtime::Handle::current(),
            warn,
            hooks,
        });
        Ok((session, reprojected))
    }

    pub fn commit_append(&self, entries: &[StoredEntry]) -> Result<(), String> {
        self.handle
            .append_sync(entries)
            .map_err(|e| e.to_string())?;
        self.generation.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    pub fn commit_rewrite(
        &self,
        header_line: &str,
        entries: Vec<StoredEntry>,
    ) -> Result<(), String> {
        let header: SessionHeader = serde_json::from_str(header_line).map_err(|e| e.to_string())?;
        if header.get("id") != self.handle.header().get("id") {
            return Err("session header ID does not match the session ID".to_owned());
        }
        self.handle
            .rewrite_sync(header, entries)
            .map_err(|e| e.to_string())?;
        self.generation.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    pub fn projection_stale(&self) -> bool {
        self.stale.load(Ordering::SeqCst)
    }

    pub fn projected(self: &Arc<Self>, session_file: &str, entry: StoredEntry) {
        let path = resolve_path(session_file);
        let session_id = self
            .handle
            .header()
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        {
            let mut st = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let offset = st.projected_size;
            st.projected_size += u64::try_from(entry.line.len()).unwrap_or(0) + 1;
            st.unrecorded_tails.push(ProjectedTail {
                key: entry.key,
                line: entry.line,
                offset,
            });
        }

        let this = Arc::clone(self);
        let path_for_err = path.clone();
        self.record_async(path_for_err, async move {
            let (tails, whole) = {
                let st = this
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let whole = if st.whole_unrecorded {
                    Some(projection_image(
                        &this.handle.header(),
                        &this.handle.entries(),
                    ))
                } else {
                    None
                };
                (st.unrecorded_tails.clone(), whole)
            };
            let written = stamp_of(Path::new(&path))
                .await
                .map_err(|e| e.to_string())?;
            let Some(written) = written else {
                return Ok(());
            };
            if let Some(whole_contents) = &whole {
                this.importer
                    .record_projection(Path::new(&path), whole_contents, written)
                    .await?;
            } else {
                this.importer
                    .record_projected_tail(Path::new(&path), &session_id, &tails, written)
                    .await?;
            }
            let mut st = this
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let drain = tails.len().min(st.unrecorded_tails.len());
            st.unrecorded_tails.drain(0..drain);
            if whole.is_some() {
                st.whole_unrecorded = false;
            }
            Ok(())
        });
    }

    pub fn record_whole(self: &Arc<Self>, session_file: &str) {
        let path = resolve_path(session_file);
        let contents = projection_image(&self.handle.header(), &self.handle.entries());
        {
            let mut st = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            st.projected_size = u64::try_from(contents.len()).unwrap_or(0);
            st.unrecorded_tails.clear();
            st.whole_unrecorded = true;
        }
        let this = Arc::clone(self);
        let path_for_err = path.clone();
        self.record_async(path_for_err, async move {
            let written = stamp_of(Path::new(&path))
                .await
                .map_err(|e| e.to_string())?;
            let Some(written) = written else {
                return Ok(());
            };
            this.importer
                .record_projection(Path::new(&path), &contents, written)
                .await?;
            let mut st = this
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            st.whole_unrecorded = false;
            Ok(())
        });
    }

    pub fn projection_failed(&self, session_file: &str, code: &str, message: &str) {
        let path = resolve_path(session_file);
        let mut unmarked = String::new();
        if !self.stale.swap(true, Ordering::SeqCst) {
            let session_id = self
                .handle
                .header()
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let mut noop = |_: &_| {};
            if let Err(err) = self.binding.store.commit_sync(
                self.binding.epoch,
                vec![StoreWrite::Put {
                    key: stale_key(&path),
                    data: json!({ "sessionId": session_id }),
                }],
                &mut noop,
            ) {
                unmarked = format!("; the stale mark was not stored ({err})");
            }
        }
        (self.warn)(ProjectionWarning {
            path,
            code: code.to_owned(),
            message: format!("{message}{unmarked}"),
        });
    }

    pub fn mark_projection_stale(self: &Arc<Self>, session_file: &str) -> Result<(), String> {
        let path = resolve_path(session_file);
        if !self.stale.swap(true, Ordering::SeqCst) {
            let session_id = self
                .handle
                .header()
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let mut noop = |_: &_| {};
            self.binding
                .store
                .commit_sync(
                    self.binding.epoch,
                    vec![StoreWrite::Put {
                        key: stale_key(&path),
                        data: json!({ "sessionId": session_id }),
                    }],
                    &mut noop,
                )
                .map_err(|e| e.to_string())?;
        }
        self.schedule_repair(&path);
        Ok(())
    }

    pub async fn settled(&self) {
        loop {
            let notified = self.pending_work.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.pending_work.count.load(Ordering::SeqCst) == 0 {
                break;
            }
            notified.await;
        }
        self.admission.admit(|| async {}).await;
    }

    pub fn schedule_repair(self: &Arc<Self>, session_file: &str) {
        let path = resolve_path(session_file);
        let this = Arc::clone(self);
        let pending = self.pending_work.track();
        self.runtime.spawn(async move {
            let _pending = pending;
            let res: Result<(), (String, String)> = this
                .admission
                .admit(|| async {
                    if !this.stale.load(Ordering::SeqCst) {
                        return Ok(());
                    }
                    let generation = this.generation.load(Ordering::SeqCst);
                    let contents = projection_image(&this.handle.header(), &this.handle.entries());
                    this.hooks
                        .before_write(&path)
                        .map_err(|e| ("EACCES".to_owned(), e))?;
                    let identity =
                        replace_with(this.binding.files.as_deref(), Path::new(&path), &contents)
                            .await
                            .map_err(|e| (e.kind().to_string(), e.to_string()))?;
                    this.hooks
                        .after_replace_with_contents(&path, identity, &contents);
                    if generation != this.generation.load(Ordering::SeqCst) {
                        this.schedule_repair(&path);
                        return Ok(());
                    }
                    let mut noop = |_: &_| {};
                    this.binding
                        .store
                        .commit_sync(
                            this.binding.epoch,
                            vec![StoreWrite::Delete {
                                key: stale_key(&path),
                            }],
                            &mut noop,
                        )
                        .map_err(|e| ("store".to_owned(), e.to_string()))?;
                    this.stale.store(false, Ordering::SeqCst);
                    {
                        let mut st = this
                            .state
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        st.projected_size = u64::try_from(contents.len()).unwrap_or(0);
                        st.unrecorded_tails.clear();
                        st.whole_unrecorded = false;
                    }
                    let written = stamp_of(Path::new(&path))
                        .await
                        .map_err(|e| (e.kind().to_string(), e.to_string()))?;
                    if let Some(written) = written
                        && generation == this.generation.load(Ordering::SeqCst)
                    {
                        this.importer
                            .record_projection(Path::new(&path), &contents, written)
                            .await
                            .map_err(|e| ("ledger".to_owned(), e))?;
                    }
                    Ok(())
                })
                .await;
            if let Err((code, msg)) = res {
                this.projection_failed(&path, &code, &msg);
            }
        });
    }

    fn record_async<F>(self: &Arc<Self>, path: String, work: F)
    where
        F: std::future::Future<Output = Result<(), String>> + Send + 'static,
    {
        let this = Arc::clone(self);
        let generation = self.generation.load(Ordering::SeqCst);
        let pending = self.pending_work.track();
        self.runtime.spawn(async move {
            let _pending = pending;
            let res = this
                .admission
                .admit(|| async {
                    if generation == this.generation.load(Ordering::SeqCst) {
                        work.await
                    } else {
                        Ok(())
                    }
                })
                .await;
            if let Err(err) = res
                && generation == this.generation.load(Ordering::SeqCst)
            {
                (this.warn)(ProjectionWarning {
                    path,
                    code: "unknown".to_owned(),
                    message: format!("projection ledger not recorded: {err}"),
                });
            }
        });
    }
}

async fn reproject_file(
    importer: &JsonlImporter,
    admission: &AdmissionLine,
    binding: &DurableStoreBinding,
    hooks: &dyn ProjectionHooks,
    path: &str,
    contents: &str,
) -> Result<(), String> {
    hooks.before_write(path)?;
    let identity = replace_with(binding.files.as_deref(), Path::new(path), contents)
        .await
        .map_err(|e| e.to_string())?;
    hooks.after_replace_with_contents(path, identity, contents);
    if let Some(written) = stamp_of(Path::new(path)).await.map_err(|e| e.to_string())? {
        admission
            .admit(|| importer.record_projection(Path::new(path), contents, written))
            .await?;
    }
    let mut noop = |_: &_| {};
    binding
        .store
        .commit_sync(
            binding.epoch,
            vec![StoreWrite::Delete {
                key: stale_key(path),
            }],
            &mut noop,
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_agent::durable::{
        DurableStore, DurableStoreOptions, create_memory_store_medium, open_memory_store,
    };

    async fn memory_binding() -> DurableStoreBinding {
        let store = Arc::new(open_memory_store(
            create_memory_store_medium(),
            DurableStoreOptions::default(),
        ));
        store.claim(0, 1).await.expect("claim");
        DurableStoreBinding {
            store,
            epoch: 1,
            files: None,
        }
    }

    struct Hooks;
    impl ProjectionHooks for Hooks {
        fn before_write(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn after_replace(&self, _: &str, _: Option<FileIdentity>) {}
    }

    #[tokio::test]
    async fn rejects_a_header_with_a_different_session_id() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp
            .path()
            .join("missing.jsonl")
            .to_string_lossy()
            .into_owned();
        let result = DurableSessionBinding::open(
            memory_binding().await,
            &path,
            "expected-id",
            r#"{"id":"other-id"}"#,
            Arc::new(|_| {}),
            Arc::new(Hooks),
        )
        .await;
        assert!(result.is_err(), "header ID must match the session key");
    }

    #[tokio::test]
    async fn imported_fork_keeps_store_and_projected_header_ids_consistent() {
        use crate::session_import::jsonl_import::{HatchMarker, JsonlFiles, write_hatch_marker};

        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("session.jsonl");
        let header = json!({"type":"session","version":3,"id":"s1","timestamp":"2026-11-24T00:00:00.000Z","cwd":"/w"});
        let first = json!({"type":"message","id":"m1","parentId":null,"timestamp":"2026-11-24T00:00:01.000Z","message":{"role":"user","content":"hi"}});
        let original = json!({"type":"message","id":"m2","parentId":"m1","timestamp":"2026-11-24T00:00:02.000Z","message":{"role":"assistant","content":"original"}});
        let changed = json!({"type":"message","id":"m3","parentId":"m1","timestamp":"2026-11-24T00:00:03.000Z","message":{"role":"assistant","content":"changed"}});
        tokio::fs::write(&file, format!("{header}\n{first}\n{original}\n"))
            .await
            .expect("initial file");
        let binding = memory_binding().await;
        let importer = create_jsonl_importer(JsonlImporterOptions {
            store: binding.store.clone(),
            epoch: binding.epoch,
            admission: create_admission_line(),
            files: None,
            now: None,
            mtime_tick_ms: Some(0.0),
        });
        importer.import_file(&file).await.expect("initial import");
        tokio::fs::write(&file, format!("{header}\n{first}\n{changed}\n"))
            .await
            .expect("divergent file");
        let stamp = RealJsonlFiles.stat(&file).await.expect("stat");
        write_hatch_marker(
            &file,
            HatchMarker {
                size: stamp.size,
                mtime_ms: stamp.mtime_ms,
            },
        )
        .await
        .expect("hatch marker");

        let store = binding.store.clone();
        let mut manager = crate::session_manager::SessionManager::open(
            file.to_str().expect("path"),
            Some(tmp.path().to_str().expect("directory")),
            Some("/w"),
            None,
        )
        .expect("open session");
        manager
            .bind_durable_store(binding)
            .await
            .expect("bind imported fork");
        let id = manager.session_id().to_owned();
        assert_ne!(id, "s1");
        let contents = tokio::fs::read_to_string(&file).await.expect("projection");
        let projected: Value =
            serde_json::from_str(contents.lines().next().expect("header")).expect("header JSON");
        assert_eq!(projected["id"], id);
        let stored = open_session_store(store, 1)
            .load(&id)
            .await
            .expect("load")
            .expect("stored fork");
        assert_eq!(
            stored.header.get("id").and_then(Value::as_str),
            Some(id.as_str())
        );
    }

    #[tokio::test]
    async fn settled_waits_for_a_scheduled_repair_before_its_first_poll() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_string_lossy().into_owned();
        let mut manager = crate::session_manager::SessionManager::create(&dir, Some(&dir), None);
        manager.persist_header_now().expect("header");
        let path = manager.session_file().expect("session file").to_owned();
        let header = tokio::fs::read_to_string(&path).await.expect("header file");
        let (binding, _) = DurableSessionBinding::open(
            memory_binding().await,
            &path,
            manager.session_id(),
            header.lines().next().expect("header"),
            Arc::new(|_| {}),
            Arc::new(Hooks),
        )
        .await
        .expect("bind");
        binding
            .commit_rewrite(header.lines().next().expect("header"), Vec::new())
            .expect("commit");
        binding
            .mark_projection_stale(&path)
            .expect("schedule repair");
        let mut settled = Box::pin(binding.settled());
        assert!(
            futures::poll!(settled.as_mut()).is_pending(),
            "scheduled repair must be awaited"
        );
        tokio::time::timeout(std::time::Duration::from_secs(5), settled)
            .await
            .expect("repair deadline");
        assert!(!binding.projection_stale());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn synchronous_session_append_works_without_a_current_runtime() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_string_lossy().into_owned();
        let mut manager = crate::session_manager::SessionManager::create(&dir, Some(&dir), None);
        manager.persist_header_now().expect("persist header");
        manager
            .bind_durable_store(memory_binding().await)
            .await
            .expect("bind");
        let path = manager.session_file().expect("session file").to_owned();
        let manager = std::thread::spawn(move || {
            manager.append_custom("runtime-probe", Some(serde_json::json!({"value": 42})));
            manager
        })
        .join()
        .expect("synchronous append must not panic");
        let contents = std::fs::read_to_string(path).expect("session file");
        assert!(
            contents
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .any(|entry| entry["type"] == "custom" && entry["customType"] == "runtime-probe")
        );
        manager.settle_durable_projection().await;
    }
}
