use crate::abort::AbortController;
use crate::abort::AbortSignal;
use crate::lsp::errors::LspError;
use crate::lsp::transport::LspClientTimeoutOptions;
use crate::lsp::transport::LspClientTransport;
use crate::lsp::types::Diagnostic;
use crate::lsp::types::ResolvedServer;
use crate::lsp::workspace_document_state::PullDiagnosticsReport;
use crate::lsp::workspace_document_state::PushDiagnosticsResolution;
use crate::lsp::workspace_document_state::WorkspaceDocumentState;
use crate::lsp::workspace_document_state::WorkspaceDocumentStateOptions;
use crate::lsp::workspace_document_state::file_uri;
use crate::lsp::workspace_edit::WorkspaceEditCommitIo;
use crate::lsp::workspace_mutation_controller::AcquireMutationLeaseResult;
use crate::lsp::workspace_mutation_controller::LspRenameResult;
use crate::lsp::workspace_mutation_controller::WorkspaceMutationController;
use crate::request_context::resolve_from;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::time::Instant;

const DIAGNOSTICS_FRESHNESS_TIMEOUT_MS: u64 = 3_000;
const VERSIONLESS_PUBLISH_QUIESCENCE_MS: f64 = 250.0;
const PRE_COMMIT_ABORT_MESSAGE: &str = "LSP request cancelled before workspace edit commit";

/// TS `LspDiagnosticsResult.transientError`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiagnosticsTransientError {
    pub kind: &'static str,
    pub message: String,
}

/// TS `LspDiagnosticsResult`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LspDiagnosticsResult {
    pub items: Vec<Diagnostic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transient_error: Option<DiagnosticsTransientError>,
}

/// TS `LspClientOptions`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LspClientOptions {
    pub timeouts: LspClientTimeoutOptions,
    pub diagnostics_freshness_timeout_ms: Option<u64>,
    pub versionless_publish_quiescence_ms: Option<f64>,
}

enum PullReport {
    Full {
        diagnostics: Vec<Diagnostic>,
        result_id: Option<String>,
    },
    Unchanged {
        result_id: Option<String>,
    },
}

/// TS `LspClient`: document sync, navigation requests, diagnostics freshness and rename.
pub struct LspClient {
    transport: Arc<LspClientTransport>,
    documents: Arc<WorkspaceDocumentState>,
    workspace_mutations: Arc<WorkspaceMutationController>,
    diagnostics_freshness_timeout_ms: u64,
    diagnostic_pull_errors: Mutex<Vec<LspError>>,
}

impl std::fmt::Debug for LspClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LspClient")
            .field("transport", &self.transport)
            .finish()
    }
}

impl LspClient {
    pub fn new(root: impl Into<String>, server: ResolvedServer, options: LspClientOptions) -> Self {
        let root = root.into();
        let transport = Arc::new(LspClientTransport::new(
            root.clone(),
            server,
            options.timeouts,
        ));
        let notify_transport = transport.clone();
        let clear_transport = transport.clone();
        let documents = Arc::new(WorkspaceDocumentState::new(
            Arc::new(move |method, params| {
                let transport = notify_transport.clone();
                Box::pin(async move { transport.send_notification(&method, Some(params)).await })
            }),
            Arc::new(move |uri| clear_transport.clear_stored_diagnostics(uri)),
            WorkspaceDocumentStateOptions {
                now: None,
                versionless_publish_quiescence_ms: Some(
                    options
                        .versionless_publish_quiescence_ms
                        .unwrap_or(VERSIONLESS_PUBLISH_QUIESCENCE_MS),
                ),
            },
        ));
        let workspace_mutations =
            Arc::new(WorkspaceMutationController::new(root, documents.clone()));
        let apply_controller = workspace_mutations.clone();
        transport.set_workspace_apply_edit_handler(Arc::new(move |params| {
            let controller = apply_controller.clone();
            Box::pin(async move { controller.handle_apply_edit(&params).await })
        }));
        let publish_documents = documents.clone();
        transport.set_publish_diagnostics_hook(Arc::new(move |params| {
            publish_documents.record_published_diagnostics(
                &params.uri,
                params.diagnostics.clone(),
                params.version,
            );
        }));
        Self {
            transport,
            documents,
            workspace_mutations,
            diagnostics_freshness_timeout_ms: options
                .diagnostics_freshness_timeout_ms
                .unwrap_or(DIAGNOSTICS_FRESHNESS_TIMEOUT_MS),
            diagnostic_pull_errors: Mutex::new(Vec::new()),
        }
    }

    pub fn transport(&self) -> &LspClientTransport {
        &self.transport
    }

    pub fn start(&self) -> Result<(), LspError> {
        self.transport.start()
    }

    pub async fn initialize(&self) -> Result<(), LspError> {
        self.transport.initialize().await
    }

    pub async fn stop(&self) {
        self.transport.stop().await;
    }

    pub fn is_alive(&self) -> bool {
        self.transport.is_alive()
    }

    pub fn pid(&self) -> Option<u32> {
        self.transport.pid()
    }

    pub fn command(&self) -> Vec<String> {
        self.transport.command()
    }

    pub fn get_diagnostic_pull_errors(&self) -> Vec<LspError> {
        self.diagnostic_pull_errors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn resolve_workspace_path(&self, file_path: &str) -> String {
        resolve_from(self.transport.root(), file_path)
    }

    pub async fn open_file(&self, file_path: &str) -> Result<(), LspError> {
        self.documents
            .open_file(&self.resolve_workspace_path(file_path))
            .await
    }

    pub fn get_open_document_version(&self, file_path: &str) -> Option<i64> {
        self.documents
            .get_version(&self.resolve_workspace_path(file_path))
    }

    pub fn get_stored_diagnostics(&self, uri: &str) -> Vec<Diagnostic> {
        self.documents.get_stored_diagnostics(uri)
    }

    pub fn set_workspace_edit_io(&self, io: WorkspaceEditCommitIo) {
        self.workspace_mutations.set_io(io);
    }

    async fn position_request(
        &self,
        method: &str,
        file_path: &str,
        line: u32,
        character: u32,
        extra: Option<(&str, Value)>,
        signal: Option<&AbortSignal>,
    ) -> Result<Value, LspError> {
        let abs_path = self.resolve_workspace_path(file_path);
        self.open_file(&abs_path).await?;
        let mut params = json!({
            "textDocument": { "uri": file_uri(&abs_path) },
            "position": { "line": i64::from(line) - 1, "character": character },
        });
        if let Some((key, value)) = extra {
            params[key] = value;
        }
        self.transport
            .send_request(method, Some(params), None, signal)
            .await
    }

    pub async fn definition(
        &self,
        file_path: &str,
        line: u32,
        character: u32,
        signal: Option<&AbortSignal>,
    ) -> Result<Value, LspError> {
        self.position_request(
            "textDocument/definition",
            file_path,
            line,
            character,
            None,
            signal,
        )
        .await
    }

    pub async fn references(
        &self,
        file_path: &str,
        line: u32,
        character: u32,
        include_declaration: bool,
        signal: Option<&AbortSignal>,
    ) -> Result<Value, LspError> {
        let context = json!({ "includeDeclaration": include_declaration });
        self.position_request(
            "textDocument/references",
            file_path,
            line,
            character,
            Some(("context", context)),
            signal,
        )
        .await
    }

    pub async fn document_symbols(
        &self,
        file_path: &str,
        signal: Option<&AbortSignal>,
    ) -> Result<Value, LspError> {
        let abs_path = self.resolve_workspace_path(file_path);
        self.open_file(&abs_path).await?;
        let params = json!({ "textDocument": { "uri": file_uri(&abs_path) } });
        self.transport
            .send_request("textDocument/documentSymbol", Some(params), None, signal)
            .await
    }

    pub async fn workspace_symbols(
        &self,
        query: &str,
        signal: Option<&AbortSignal>,
    ) -> Result<Value, LspError> {
        self.transport
            .send_request(
                "workspace/symbol",
                Some(json!({ "query": query })),
                None,
                signal,
            )
            .await
    }

    pub async fn prepare_rename(
        &self,
        file_path: &str,
        line: u32,
        character: u32,
        signal: Option<&AbortSignal>,
    ) -> Result<Value, LspError> {
        self.position_request(
            "textDocument/prepareRename",
            file_path,
            line,
            character,
            None,
            signal,
        )
        .await
    }

    fn freshness_timeout(&self, abs_path: &str) -> LspDiagnosticsResult {
        LspDiagnosticsResult {
            items: Vec::new(),
            transient_error: Some(DiagnosticsTransientError {
                kind: "freshness_timeout",
                message: format!(
                    "Timed out waiting for fresh diagnostics for {abs_path} within {}ms.",
                    self.diagnostics_freshness_timeout_ms
                ),
            }),
        }
    }

    /// TS `diagnostics`: prefers a version-matched push publish, otherwise pulls
    /// `textDocument/diagnostic` until the freshness deadline, falling back to push waits.
    pub async fn diagnostics(
        &self,
        file_path: &str,
        signal: Option<&AbortSignal>,
    ) -> Result<LspDiagnosticsResult, LspError> {
        throw_if_aborted(signal)?;
        let abs_path = self.resolve_workspace_path(file_path);
        let uri = file_uri(&abs_path);
        self.open_file(&abs_path).await?;
        let started = Instant::now();
        let deadline_ms = self.diagnostics_freshness_timeout_ms as f64;
        let remaining_ms = || deadline_ms - started.elapsed().as_secs_f64() * 1000.0;

        loop {
            throw_if_aborted(signal)?;
            let Some(snapshot) = self.documents.capture_diagnostic_snapshot(&abs_path) else {
                return Ok(self.freshness_timeout(&abs_path));
            };
            let push = self.documents.resolve_push_diagnostics(&snapshot);
            if let PushDiagnosticsResolution::Ready(items) = push {
                return Ok(LspDiagnosticsResult {
                    items,
                    transient_error: None,
                });
            }

            let mut push_fallback_only = !self.transport.is_diagnostic_pull_supported();
            if !push_fallback_only {
                let cached = self.documents.get_pull_cache(&snapshot);
                let remaining = remaining_ms();
                if remaining <= 0.0 {
                    return Ok(self.freshness_timeout(&abs_path));
                }
                let mut params = json!({ "textDocument": { "uri": uri } });
                if let Some(result_id) = cached.as_ref().and_then(|cached| cached.result_id.clone())
                {
                    params["previousResultId"] = json!(result_id);
                }
                let response = self
                    .transport
                    .send_request(
                        "textDocument/diagnostic",
                        Some(params),
                        Some(remaining.ceil() as u64),
                        signal,
                    )
                    .await;
                match response {
                    Ok(result) => {
                        if !self.documents.is_current_snapshot(&snapshot) {
                            continue;
                        }
                        match parse_diagnostic_pull_report(&result) {
                            PullReport::Full {
                                diagnostics,
                                result_id,
                            } => {
                                self.documents.record_pull_diagnostics(
                                    &snapshot,
                                    PullDiagnosticsReport {
                                        diagnostics: diagnostics.clone(),
                                        result_id,
                                    },
                                );
                                return Ok(LspDiagnosticsResult {
                                    items: diagnostics,
                                    transient_error: None,
                                });
                            }
                            PullReport::Unchanged { result_id } => {
                                if let Some(cached) = cached.filter(|cached| {
                                    cached.document_version == snapshot.version
                                        && cached.result_id == result_id
                                }) {
                                    return Ok(LspDiagnosticsResult {
                                        items: cached.diagnostics,
                                        transient_error: None,
                                    });
                                }
                            }
                        }
                    }
                    Err(error) if is_unsupported_diagnostic_pull_error(&error) => {
                        self.transport.set_diagnostic_pull_supported(false);
                        push_fallback_only = true;
                    }
                    Err(LspError::RequestTimeout { .. }) => push_fallback_only = true,
                    Err(error) => {
                        self.diagnostic_pull_errors
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .push(error.clone());
                        return Err(error);
                    }
                }
            }

            if !push_fallback_only {
                continue;
            }
            let remaining = remaining_ms();
            if remaining <= 0.0 {
                // A server that never advertised (or rejected) pull diagnostics and never
                // published for this document stays silent for a clean file (Marksman,
                // #6131): return the version-current pull cache or an explicit empty list.
                if !self.transport.is_diagnostic_pull_supported()
                    && snapshot.publish_generation == 0
                {
                    let items = self
                        .documents
                        .get_pull_cache(&snapshot)
                        .map(|cached| cached.diagnostics)
                        .unwrap_or_default();
                    return Ok(LspDiagnosticsResult {
                        items,
                        transient_error: None,
                    });
                }
                return Ok(self.freshness_timeout(&abs_path));
            }
            let wait_ms = match push {
                PushDiagnosticsResolution::Wait(wait_ms) => wait_ms.min(remaining),
                _ => remaining,
            };
            let wait = self
                .documents
                .wait_for_diagnostics_activity(&snapshot, wait_ms);
            match signal {
                None => wait.await,
                Some(signal) => {
                    tokio::select! {
                        () = wait => {}
                        () = signal.cancelled() => return Err(abort_error(signal)),
                    }
                }
            }
        }
    }

    /// TS `rename`: holds a workspace mutation lease for the request; a caller abort
    /// cancels the request only while no server-initiated apply has started.
    pub async fn rename(
        &self,
        file_path: &str,
        line: u32,
        character: u32,
        new_name: &str,
        signal: Option<&AbortSignal>,
    ) -> Result<LspRenameResult, LspError> {
        let abs_path = self.resolve_workspace_path(file_path);
        self.open_file(&abs_path).await?;
        let lease = match self.workspace_mutations.acquire(signal) {
            AcquireMutationLeaseResult::Success(lease) => lease,
            AcquireMutationLeaseResult::Failure(result) => {
                return Ok(LspRenameResult {
                    edit: None,
                    apply: result,
                });
            }
        };
        let pre_commit = signal.map(|source| {
            let controller = AbortController::new();
            let target = controller.clone();
            let mutations = self.workspace_mutations.clone();
            let on_abort = move |reason: &LspError| {
                if mutations.is_before_commit(lease) {
                    target.abort_with(pre_commit_abort_reason(reason));
                }
            };
            let listener = match source.reason() {
                Some(reason) => {
                    on_abort(&reason);
                    None
                }
                None => Some(source.on_abort(on_abort)),
            };
            (source, controller, listener)
        });
        let params = json!({
            "textDocument": { "uri": file_uri(&abs_path) },
            "position": { "line": i64::from(line) - 1, "character": character },
            "newName": new_name,
        });
        let pre_commit_signal = pre_commit
            .as_ref()
            .map(|(_, controller, _)| controller.signal());
        let response = self
            .transport
            .send_request(
                "textDocument/rename",
                Some(params),
                None,
                pre_commit_signal.as_ref(),
            )
            .await;
        let result = match response {
            Ok(edit) => {
                let edit = (!edit.is_null()).then_some(edit);
                Ok(self.workspace_mutations.reconcile_rename(lease, edit).await)
            }
            Err(error) => Err(error),
        };
        if let Some((source, _, Some(listener))) = pre_commit {
            source.remove_listener(listener);
        }
        self.workspace_mutations.release(lease);
        result
    }
}

fn throw_if_aborted(signal: Option<&AbortSignal>) -> Result<(), LspError> {
    signal.map_or(Ok(()), AbortSignal::throw_if_aborted)
}

fn abort_error(signal: &AbortSignal) -> LspError {
    signal.reason().unwrap_or(LspError::Aborted)
}

fn pre_commit_abort_reason(reason: &LspError) -> LspError {
    if matches!(reason, LspError::Aborted) {
        LspError::other(PRE_COMMIT_ABORT_MESSAGE)
    } else {
        reason.clone()
    }
}

fn is_unsupported_diagnostic_pull_error(error: &LspError) -> bool {
    if matches!(
        error,
        LspError::JsonRpc {
            code: Some(-32601),
            ..
        }
    ) {
        return true;
    }
    let message = error.to_string().to_lowercase();
    [
        "unsupported",
        "not supported",
        "method not found",
        "unknown request",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

fn parse_diagnostic_pull_report(value: &Value) -> PullReport {
    let result_id = value
        .get("resultId")
        .and_then(Value::as_str)
        .map(str::to_string);
    if value.get("kind").and_then(Value::as_str) == Some("unchanged") {
        return PullReport::Unchanged { result_id };
    }
    let diagnostics = value
        .get("items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| serde_json::from_value(item.clone()).ok())
                .collect()
        })
        .unwrap_or_default();
    PullReport::Full {
        diagnostics,
        result_id,
    }
}
