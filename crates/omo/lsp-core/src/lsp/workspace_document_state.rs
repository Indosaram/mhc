use crate::lsp::effective_extension::effective_extension;
use crate::lsp::errors::LspError;
use crate::lsp::json_rpc_connection::BoxFuture;
use crate::lsp::language_mappings::get_language_id;
use crate::lsp::types::Diagnostic;
use crate::lsp::workspace_edit::PlannedWorkspaceOperation;
use crate::lsp::workspace_edit::WorkspaceMutation;
use crate::lsp::workspace_edit::WorkspaceMutationDelta;
use crate::request_context::resolve;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::time::Duration;
use tokio::sync::watch;

const WATCHED_FILE_BATCH_SIZE: usize = 128;
const DEFAULT_VERSIONLESS_PUBLISH_QUIESCENCE_MS: f64 = 250.0;

pub type SendNotificationFn =
    Arc<dyn Fn(String, Value) -> BoxFuture<Result<(), LspError>> + Send + Sync>;
pub type ClearDiagnosticsFn = Arc<dyn Fn(&str) + Send + Sync>;
pub type NowFn = Arc<dyn Fn() -> f64 + Send + Sync>;

#[derive(Debug, Clone)]
struct PublishedDiagnostics {
    diagnostics: Vec<Diagnostic>,
    document_generation_at_arrival: u64,
    arrived_at: f64,
    version: Option<f64>,
}

/// TS `PullDiagnosticsCacheHit`.
#[derive(Debug, Clone, PartialEq)]
pub struct PullDiagnosticsCacheHit {
    pub document_version: i64,
    pub diagnostics: Vec<Diagnostic>,
    pub result_id: Option<String>,
}

/// TS `PullDiagnosticsReport` (kind is always `"full"`).
#[derive(Debug, Clone, PartialEq)]
pub struct PullDiagnosticsReport {
    pub diagnostics: Vec<Diagnostic>,
    pub result_id: Option<String>,
}

#[derive(Debug)]
struct OpenDocumentState {
    path: String,
    uri: String,
    text: String,
    version: i64,
    generation: u64,
    publish_generation: u64,
    last_publish: Option<PublishedDiagnostics>,
    pull_cache: Option<PullDiagnosticsCacheHit>,
    activity: watch::Sender<u64>,
}

impl OpenDocumentState {
    fn notify_waiters(&self) {
        self.activity.send_modify(|count| *count += 1);
    }
}

/// TS `DocumentVersionFailure`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentVersionFailure {
    pub change_index: usize,
    pub message: String,
}

/// TS `DiagnosticSnapshot`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticSnapshot {
    pub path: String,
    pub uri: String,
    pub version: i64,
    pub document_generation: u64,
    pub publish_generation: u64,
}

/// TS `PushDiagnosticsResolution`.
#[derive(Debug, Clone, PartialEq)]
pub enum PushDiagnosticsResolution {
    Ready(Vec<Diagnostic>),
    Wait(f64),
    Missing,
}

/// TS `WorkspaceDocumentStateOptions`.
#[derive(Clone, Default)]
pub struct WorkspaceDocumentStateOptions {
    pub now: Option<NowFn>,
    pub versionless_publish_quiescence_ms: Option<f64>,
}

#[derive(Default)]
struct Documents {
    by_path: HashMap<String, OpenDocumentState>,
    path_by_uri: HashMap<String, String>,
}

impl Documents {
    fn by_uri(&self, uri: &str) -> Option<&OpenDocumentState> {
        self.path_by_uri
            .get(uri)
            .and_then(|path| self.by_path.get(path))
    }

    fn by_uri_mut(&mut self, uri: &str) -> Option<&mut OpenDocumentState> {
        let path = self.path_by_uri.get(uri)?;
        self.by_path.get_mut(path)
    }
}

/// TS `WorkspaceDocumentState`: open-document versions, diagnostics caches and
/// post-commit synchronization notifications.
pub struct WorkspaceDocumentState {
    documents: Mutex<Documents>,
    open_gates: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    send_notification: SendNotificationFn,
    clear_diagnostics: ClearDiagnosticsFn,
    now: NowFn,
    versionless_publish_quiescence_ms: f64,
}

impl std::fmt::Debug for WorkspaceDocumentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceDocumentState")
            .field("open_documents", &self.lock().by_path.len())
            .finish()
    }
}

pub fn canonical_path(file_path: &str) -> String {
    let absolute = resolve(file_path);
    std::fs::canonicalize(&absolute).map_or(absolute, |path| path.to_string_lossy().into_owned())
}

fn is_same_or_descendant(candidate: &str, parent: &str) -> bool {
    Path::new(candidate).starts_with(parent)
}

fn moved_path(candidate: &str, old_path: &str, new_path: &str) -> String {
    match Path::new(candidate).strip_prefix(old_path) {
        Ok(suffix) if suffix.as_os_str().is_empty() => new_path.to_string(),
        Ok(suffix) => resolve(&Path::new(new_path).join(suffix).to_string_lossy()),
        Err(_) => new_path.to_string(),
    }
}

pub fn file_uri(path: &str) -> String {
    url::Url::from_file_path(path).map_or_else(|()| format!("file://{path}"), |url| url.to_string())
}

fn system_now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1000.0)
}

fn read_text(path: &str) -> Result<String, LspError> {
    std::fs::read_to_string(path).map_err(LspError::from)
}

impl WorkspaceDocumentState {
    pub fn new(
        send_notification: SendNotificationFn,
        clear_diagnostics: ClearDiagnosticsFn,
        options: WorkspaceDocumentStateOptions,
    ) -> Self {
        Self {
            documents: Mutex::new(Documents::default()),
            open_gates: Mutex::new(HashMap::new()),
            send_notification,
            clear_diagnostics,
            now: options.now.unwrap_or_else(|| Arc::new(system_now_ms)),
            versionless_publish_quiescence_ms: options
                .versionless_publish_quiescence_ms
                .unwrap_or(DEFAULT_VERSIONLESS_PUBLISH_QUIESCENCE_MS),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Documents> {
        self.documents
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), LspError> {
        (self.send_notification)(method.to_string(), params).await
    }

    fn open_gate(&self, path: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.open_gates
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(path.to_string())
            .or_default()
            .clone()
    }

    /// TS `openFile`: opens once (single flight per path) or pushes a full-text change.
    pub async fn open_file(&self, file_path: &str) -> Result<(), LspError> {
        let path = canonical_path(file_path);
        let gate = self.open_gate(&path);
        let _guard = gate.lock().await;
        let text = read_text(&path)?;
        let changed = {
            let documents = self.lock();
            match documents.by_path.get(&path) {
                None => None,
                Some(existing) if existing.text == text => return Ok(()),
                Some(_) => Some(()),
            }
        };
        match changed {
            None => self.open_document(&path, text).await,
            Some(()) => self.change_document(&path, text).await,
        }
    }

    pub fn get_version(&self, file_path: &str) -> Option<i64> {
        self.lock()
            .by_path
            .get(&canonical_path(file_path))
            .map(|state| state.version)
    }

    pub fn get_stored_diagnostics(&self, uri: &str) -> Vec<Diagnostic> {
        let documents = self.lock();
        let Some(state) = documents.by_uri(uri) else {
            return Vec::new();
        };
        state
            .last_publish
            .as_ref()
            .map(|publish| publish.diagnostics.clone())
            .or_else(|| {
                state
                    .pull_cache
                    .as_ref()
                    .map(|cache| cache.diagnostics.clone())
            })
            .unwrap_or_default()
    }

    pub fn capture_diagnostic_snapshot(&self, file_path: &str) -> Option<DiagnosticSnapshot> {
        let documents = self.lock();
        let state = documents.by_path.get(&canonical_path(file_path))?;
        Some(DiagnosticSnapshot {
            path: state.path.clone(),
            uri: state.uri.clone(),
            version: state.version,
            document_generation: state.generation,
            publish_generation: state.publish_generation,
        })
    }

    pub fn is_current_snapshot(&self, snapshot: &DiagnosticSnapshot) -> bool {
        self.lock()
            .by_path
            .get(&snapshot.path)
            .is_some_and(|state| {
                state.uri == snapshot.uri
                    && state.version == snapshot.version
                    && state.generation == snapshot.document_generation
            })
    }

    pub fn get_pull_cache(&self, snapshot: &DiagnosticSnapshot) -> Option<PullDiagnosticsCacheHit> {
        let documents = self.lock();
        let cache = documents.by_uri(&snapshot.uri)?.pull_cache.as_ref()?;
        (cache.document_version == snapshot.version).then(|| cache.clone())
    }

    pub fn record_pull_diagnostics(
        &self,
        snapshot: &DiagnosticSnapshot,
        report: PullDiagnosticsReport,
    ) {
        if let Some(state) = self.lock().by_uri_mut(&snapshot.uri) {
            state.pull_cache = Some(PullDiagnosticsCacheHit {
                document_version: snapshot.version,
                diagnostics: report.diagnostics,
                result_id: report.result_id,
            });
        }
    }

    pub fn record_published_diagnostics(
        &self,
        uri: &str,
        diagnostics: Vec<Diagnostic>,
        version: Option<f64>,
    ) {
        let now = (self.now)();
        let mut documents = self.lock();
        let Some(state) = documents.by_uri_mut(uri) else {
            return;
        };
        state.publish_generation += 1;
        state.last_publish = Some(PublishedDiagnostics {
            diagnostics,
            document_generation_at_arrival: state.generation,
            arrived_at: now,
            version,
        });
        state.notify_waiters();
    }

    pub fn resolve_push_diagnostics(
        &self,
        snapshot: &DiagnosticSnapshot,
    ) -> PushDiagnosticsResolution {
        let now = (self.now)();
        let documents = self.lock();
        let Some(publish) = documents
            .by_uri(&snapshot.uri)
            .and_then(|state| state.last_publish.as_ref())
        else {
            return PushDiagnosticsResolution::Missing;
        };
        if let Some(version) = publish.version {
            return if version == snapshot.version as f64 {
                PushDiagnosticsResolution::Ready(publish.diagnostics.clone())
            } else {
                PushDiagnosticsResolution::Missing
            };
        }
        if publish.document_generation_at_arrival < snapshot.document_generation {
            return PushDiagnosticsResolution::Missing;
        }
        let wait_ms = (publish.arrived_at + self.versionless_publish_quiescence_ms - now).max(0.0);
        if wait_ms == 0.0 {
            PushDiagnosticsResolution::Ready(publish.diagnostics.clone())
        } else {
            PushDiagnosticsResolution::Wait(wait_ms)
        }
    }

    /// TS `waitForDiagnosticsActivity`: resolves on the next document/diagnostic event or timeout.
    pub async fn wait_for_diagnostics_activity(
        &self,
        snapshot: &DiagnosticSnapshot,
        timeout_ms: f64,
    ) {
        if timeout_ms <= 0.0 {
            return;
        }
        let mut receiver = {
            let documents = self.lock();
            let Some(state) = documents.by_uri(&snapshot.uri) else {
                return;
            };
            state.activity.subscribe()
        };
        let _ = tokio::time::timeout(
            Duration::from_secs_f64(timeout_ms / 1000.0),
            receiver.changed(),
        )
        .await;
    }

    /// TS `validateVersions`: simulates version bumps through the planned operations.
    pub fn validate_versions(
        &self,
        operations: &[PlannedWorkspaceOperation],
    ) -> Option<DocumentVersionFailure> {
        let mut versions: HashMap<String, i64> = self
            .lock()
            .by_path
            .iter()
            .map(|(path, state)| (path.clone(), state.version))
            .collect();
        for operation in operations {
            match operation {
                PlannedWorkspaceOperation::Text {
                    change_index,
                    path,
                    document_version,
                    ..
                } => {
                    let current = versions.get(path).copied();
                    if let Some(expected) = document_version
                        && current != Some(*expected)
                    {
                        let observed = current.map_or_else(
                            || "closed document".to_string(),
                            |current| format!("open document version {current}"),
                        );
                        return Some(DocumentVersionFailure {
                            change_index: *change_index,
                            message: format!(
                                "document version {expected} does not match {observed} for {path}"
                            ),
                        });
                    }
                    if let Some(current) = current {
                        versions.insert(path.clone(), current + 1);
                    }
                }
                PlannedWorkspaceOperation::Rename {
                    old_path, new_path, ..
                } => {
                    let moved: Vec<String> = versions
                        .keys()
                        .filter(|path| is_same_or_descendant(path, old_path))
                        .cloned()
                        .collect();
                    for path in &moved {
                        versions.remove(path);
                    }
                    for path in &moved {
                        versions.insert(moved_path(path, old_path, new_path), 1);
                    }
                }
                PlannedWorkspaceOperation::Delete { path, .. } => {
                    versions.retain(|candidate, _| !is_same_or_descendant(candidate, path));
                }
                PlannedWorkspaceOperation::Create { path, replaced, .. } => {
                    if *replaced && versions.contains_key(path) {
                        versions.insert(path.clone(), 1);
                    }
                }
                PlannedWorkspaceOperation::Noop { .. } => {}
            }
        }
        None
    }

    /// TS `synchronize`: open documents get didChange/didClose/didOpen; closed files are
    /// reported through `workspace/didChangeWatchedFiles` in batches of 128.
    pub async fn synchronize(&self, delta: &WorkspaceMutationDelta) -> Result<(), LspError> {
        let mut watched: Vec<Value> = Vec::new();
        for mutation in &delta.operations {
            self.synchronize_mutation(mutation, &mut watched).await?;
        }
        for batch in watched.chunks(WATCHED_FILE_BATCH_SIZE) {
            self.notify(
                "workspace/didChangeWatchedFiles",
                json!({ "changes": batch }),
            )
            .await?;
        }
        Ok(())
    }

    fn open_paths_under(&self, parent: &str) -> Vec<String> {
        let mut paths: Vec<String> = self
            .lock()
            .by_path
            .keys()
            .filter(|path| is_same_or_descendant(path, parent))
            .cloned()
            .collect();
        paths.sort();
        paths
    }

    async fn synchronize_mutation(
        &self,
        mutation: &WorkspaceMutation,
        watched: &mut Vec<Value>,
    ) -> Result<(), LspError> {
        let event = |path: &str, kind: u8| json!({ "uri": file_uri(path), "type": kind });
        match mutation {
            WorkspaceMutation::Text {
                path, after_text, ..
            } => {
                let is_open = self.lock().by_path.contains_key(path);
                if is_open {
                    self.change_document(path, after_text.clone()).await?;
                } else {
                    watched.push(event(path, 2));
                }
            }
            WorkspaceMutation::Create { path, replaced } => {
                let is_open = self.lock().by_path.contains_key(path);
                if is_open {
                    self.close_document(path).await?;
                    self.open_document(path, read_text(path)?).await?;
                } else {
                    watched.push(event(path, if *replaced { 2 } else { 1 }));
                }
            }
            WorkspaceMutation::Rename {
                old_path, new_path, ..
            } => {
                let moved = self.open_paths_under(old_path);
                for path in &moved {
                    self.close_document(path).await?;
                }
                for path in &moved {
                    let destination = moved_path(path, old_path, new_path);
                    self.open_document(&destination, read_text(&destination)?)
                        .await?;
                }
                if moved.is_empty() {
                    watched.push(event(old_path, 3));
                    watched.push(event(new_path, 1));
                }
            }
            WorkspaceMutation::Delete { path, .. } => {
                let removed = self.open_paths_under(path);
                for open in &removed {
                    self.close_document(open).await?;
                }
                if removed.is_empty() {
                    watched.push(event(path, 3));
                }
            }
        }
        Ok(())
    }

    async fn open_document(&self, path: &str, text: String) -> Result<(), LspError> {
        let uri = file_uri(path);
        let language_id = get_language_id(&effective_extension(path));
        {
            let mut documents = self.lock();
            if documents.by_path.contains_key(path) {
                return Ok(());
            }
            let state = OpenDocumentState {
                path: path.to_string(),
                uri: uri.clone(),
                text: text.clone(),
                version: 1,
                generation: 1,
                publish_generation: 0,
                last_publish: None,
                pull_cache: None,
                activity: watch::channel(0).0,
            };
            state.notify_waiters();
            documents.path_by_uri.insert(uri.clone(), path.to_string());
            documents.by_path.insert(path.to_string(), state);
        }
        self.notify(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": language_id, "version": 1, "text": text } }),
        )
        .await
    }

    async fn change_document(&self, path: &str, text: String) -> Result<(), LspError> {
        let (uri, version) = {
            let mut documents = self.lock();
            let Some(state) = documents.by_path.get_mut(path) else {
                return Ok(());
            };
            state.text = text.clone();
            state.version += 1;
            state.generation += 1;
            state.notify_waiters();
            (state.uri.clone(), state.version)
        };
        (self.clear_diagnostics)(&uri);
        self.notify(
            "textDocument/didChange",
            json!({ "textDocument": { "uri": uri, "version": version }, "contentChanges": [{ "text": text }] }),
        )
        .await?;
        self.notify(
            "textDocument/didSave",
            json!({ "textDocument": { "uri": uri }, "text": text }),
        )
        .await
    }

    async fn close_document(&self, path: &str) -> Result<(), LspError> {
        let uri = {
            let mut documents = self.lock();
            let Some(state) = documents.by_path.remove(path) else {
                return Ok(());
            };
            documents.path_by_uri.remove(&state.uri);
            state.notify_waiters();
            state.uri
        };
        (self.clear_diagnostics)(&uri);
        self.notify(
            "textDocument/didClose",
            json!({ "textDocument": { "uri": uri } }),
        )
        .await
    }
}

#[cfg(test)]
#[path = "workspace_document_state_tests.rs"]
mod tests;
