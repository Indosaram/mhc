use crate::abort::AbortController;
use crate::abort::AbortSignal;
use crate::lsp::cleanup_errors::report_best_effort_cleanup_error;
use crate::lsp::constants::INIT_TIMEOUT_MS;
use crate::lsp::constants::REQUEST_TIMEOUT_MS;
use crate::lsp::constants::STOP_HARD_KILL_TIMEOUT_MS;
use crate::lsp::constants::STOP_SIGKILL_GRACE_MS;
use crate::lsp::errors::LspError;
use crate::lsp::json_rpc_connection::BoxFuture;
use crate::lsp::json_rpc_connection::JsonRpcConnection;
use crate::lsp::process::KillSignal;
use crate::lsp::process::SpawnOptions;
use crate::lsp::process::SpawnedProcess;
use crate::lsp::process::spawn_process;
use crate::lsp::transport_protocol::DiagnosticsParams;
use crate::lsp::transport_protocol::create_lsp_spawn_env;
use crate::lsp::transport_protocol::parse_configuration_items;
use crate::lsp::transport_protocol::parse_diagnostics_params;
use crate::lsp::types::Diagnostic;
use crate::lsp::types::ResolvedServer;
use crate::lsp::workspace_mutation_controller::WorkspaceApplyEditResponse;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::io::AsyncReadExt;

const STDERR_CHUNK_LIMIT: usize = 100;

/// TS `LspClientTimeoutOptions`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LspClientTimeoutOptions {
    pub request_timeout_ms: Option<u64>,
    pub initialize_timeout_ms: Option<u64>,
}

pub type WorkspaceApplyEditHandler =
    Arc<dyn Fn(Value) -> BoxFuture<WorkspaceApplyEditResponse> + Send + Sync>;
pub type PublishDiagnosticsHook = Arc<dyn Fn(&DiagnosticsParams) + Send + Sync>;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// TS `LspClientTransport` + `LspClientConnection`: owns the server process, the
/// JSON-RPC connection, stderr capture and the raw push-diagnostics store.
pub struct LspClientTransport {
    root: String,
    server: ResolvedServer,
    request_timeout_ms: u64,
    initialize_timeout_ms: u64,
    process: Mutex<Option<SpawnedProcess>>,
    connection: Mutex<Option<JsonRpcConnection>>,
    stderr_buffer: Arc<Mutex<VecDeque<String>>>,
    process_exited: Arc<AtomicBool>,
    diagnostics_store: Arc<Mutex<HashMap<String, Vec<Diagnostic>>>>,
    workspace_apply_edit_handler: Mutex<Option<WorkspaceApplyEditHandler>>,
    publish_diagnostics_hook: Mutex<Option<PublishDiagnosticsHook>>,
    diagnostic_pull_supported: AtomicBool,
}

impl std::fmt::Debug for LspClientTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LspClientTransport")
            .field("root", &self.root)
            .field("server", &self.server.id)
            .finish()
    }
}

impl LspClientTransport {
    pub fn new(
        root: impl Into<String>,
        server: ResolvedServer,
        timeouts: LspClientTimeoutOptions,
    ) -> Self {
        Self {
            root: root.into(),
            server,
            request_timeout_ms: timeouts.request_timeout_ms.unwrap_or(REQUEST_TIMEOUT_MS),
            initialize_timeout_ms: timeouts.initialize_timeout_ms.unwrap_or(INIT_TIMEOUT_MS),
            process: Mutex::new(None),
            connection: Mutex::new(None),
            stderr_buffer: Arc::default(),
            process_exited: Arc::new(AtomicBool::new(false)),
            diagnostics_store: Arc::default(),
            workspace_apply_edit_handler: Mutex::new(None),
            publish_diagnostics_hook: Mutex::new(None),
            diagnostic_pull_supported: AtomicBool::new(false),
        }
    }

    pub fn root(&self) -> &str {
        &self.root
    }

    pub fn server(&self) -> &ResolvedServer {
        &self.server
    }

    pub fn initialize_timeout_ms(&self) -> u64 {
        self.initialize_timeout_ms
    }

    pub fn pid(&self) -> Option<u32> {
        lock(&self.process).as_ref().and_then(|process| process.pid)
    }

    pub fn command(&self) -> Vec<String> {
        self.server.command.clone()
    }

    pub fn set_workspace_apply_edit_handler(&self, handler: WorkspaceApplyEditHandler) {
        *lock(&self.workspace_apply_edit_handler) = Some(handler);
    }

    pub fn has_workspace_apply_edit_handler(&self) -> bool {
        lock(&self.workspace_apply_edit_handler).is_some()
    }

    pub fn set_publish_diagnostics_hook(&self, hook: PublishDiagnosticsHook) {
        *lock(&self.publish_diagnostics_hook) = Some(hook);
    }

    pub fn set_diagnostic_pull_supported(&self, supported: bool) {
        self.diagnostic_pull_supported
            .store(supported, Ordering::SeqCst);
    }

    pub fn is_diagnostic_pull_supported(&self) -> bool {
        self.diagnostic_pull_supported.load(Ordering::SeqCst)
    }

    pub fn clear_stored_diagnostics(&self, uri: &str) {
        lock(&self.diagnostics_store).remove(uri);
    }

    pub fn get_stored_diagnostics(&self, uri: &str) -> Vec<Diagnostic> {
        lock(&self.diagnostics_store)
            .get(uri)
            .cloned()
            .unwrap_or_default()
    }

    fn stderr_tail(&self, count: usize) -> Option<String> {
        let buffer = lock(&self.stderr_buffer);
        let skip = buffer.len().saturating_sub(count);
        let tail = buffer
            .iter()
            .skip(skip)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        (!tail.is_empty()).then_some(tail)
    }

    fn exit_code(&self) -> Option<i32> {
        lock(&self.process)
            .as_mut()
            .and_then(SpawnedProcess::exit_code)
    }

    fn has_exited(&self) -> bool {
        self.process_exited.load(Ordering::SeqCst) || self.exit_code().is_some()
    }

    fn process_exited_error(&self) -> LspError {
        LspError::ProcessExited {
            server_id: self.server.id.clone(),
            root: self.root.clone(),
            exit_code: self.exit_code(),
            stderr_tail: self.stderr_tail(10),
        }
    }

    pub fn is_alive(&self) -> bool {
        lock(&self.process).is_some() && !self.has_exited()
    }

    /// TS `start`: spawns the server and wires the client-side request handlers.
    pub fn start(&self) -> Result<(), LspError> {
        let mut env: BTreeMap<String, String> = std::env::vars().collect();
        if let Some(server_env) = &self.server.env {
            env.extend(server_env.clone());
        }
        let env = create_lsp_spawn_env(&self.root, &env);
        let mut process = spawn_process(
            &self.server.command,
            &SpawnOptions {
                cwd: self.root.clone(),
                env,
            },
        )?;
        if let Some(stderr) = process.stderr.take() {
            tokio::spawn(read_stderr(stderr, self.stderr_buffer.clone()));
        }
        if let Some(exit_code) = process.exit_code() {
            let stderr = lock(&self.stderr_buffer)
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");
            let skip = stderr.chars().count().saturating_sub(2000);
            return Err(LspError::ProcessExited {
                server_id: self.server.id.clone(),
                root: self.root.clone(),
                exit_code: Some(exit_code),
                stderr_tail: Some(stderr.chars().skip(skip).collect()),
            });
        }
        let (Some(stdout), Some(stdin)) = (process.stdout.take(), process.stdin.take()) else {
            return Err(LspError::ProcessSpawn(
                "Spawned process is missing one of stdin/stdout/stderr pipes".to_string(),
            ));
        };
        *lock(&self.process) = Some(process);
        let connection = JsonRpcConnection::new(stdout, stdin);

        let store = self.diagnostics_store.clone();
        let hook = lock(&self.publish_diagnostics_hook).clone();
        connection.on_notification("textDocument/publishDiagnostics", move |params| {
            if let Some(params) =
                parse_diagnostics_params(&params).filter(|params| !params.uri.is_empty())
            {
                lock(&store).insert(params.uri.clone(), params.diagnostics.clone());
                if let Some(hook) = &hook {
                    hook(&params);
                }
            }
            Ok(())
        });
        connection.on_request("workspace/configuration", |params| async move {
            let items: Vec<Value> = parse_configuration_items(&params)
                .into_iter()
                .map(|item| match item.section.as_deref() {
                    Some("json") => json!({ "validate": { "enable": true } }),
                    _ => json!({}),
                })
                .collect();
            Ok(Value::Array(items))
        });
        connection.on_request("client/registerCapability", |_| async { Ok(Value::Null) });
        connection.on_request("window/workDoneProgress/create", |_| async {
            Ok(Value::Null)
        });
        if let Some(handler) = lock(&self.workspace_apply_edit_handler).clone() {
            connection.on_request("workspace/applyEdit", move |params| {
                let response = handler(params);
                async move { Ok(serde_json::to_value(response.await).unwrap_or(Value::Null)) }
            });
        }
        let exited = self.process_exited.clone();
        connection.on_close(move || exited.store(true, Ordering::SeqCst));
        connection.on_error(|error| {
            report_best_effort_cleanup_error("connection error notification", error)
        });
        connection.listen();
        *lock(&self.connection) = Some(connection);
        Ok(())
    }

    /// TS `sendRequest`: per-request timeout plus optional caller signal, with
    /// process-exit and connection-closed error mapping.
    pub async fn send_request(
        &self,
        method: &str,
        params: Option<Value>,
        timeout_ms: Option<u64>,
        signal: Option<&AbortSignal>,
    ) -> Result<Value, LspError> {
        let Some(connection) = lock(&self.connection).clone() else {
            return Err(LspError::NotStarted {
                server_id: self.server.id.clone(),
                root: self.root.clone(),
            });
        };
        if self.has_exited() {
            return Err(self.process_exited_error());
        }
        let combined = AbortController::new();
        let listener = signal.map(|signal| {
            if let Some(reason) = signal.reason() {
                combined.abort_with(reason);
            }
            let target = combined.clone();
            (
                signal,
                signal.on_abort(move |reason| target.abort_with(reason.clone())),
            )
        });
        let timer = {
            let target = combined.clone();
            let stderr = self.stderr_buffer.clone();
            let method = method.to_string();
            let timeout = Duration::from_millis(timeout_ms.unwrap_or(self.request_timeout_ms));
            tokio::spawn(async move {
                tokio::time::sleep(timeout).await;
                let tail = {
                    let buffer = lock(&stderr);
                    let skip = buffer.len().saturating_sub(5);
                    buffer
                        .iter()
                        .skip(skip)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                target.abort_with(LspError::RequestTimeout {
                    method,
                    stderr_tail: (!tail.is_empty()).then_some(tail),
                });
            })
        };
        let result = connection
            .send_request(method, params, Some(&combined.signal()))
            .await;
        timer.abort();
        if let Some((signal, id)) = listener {
            signal.remove_listener(id);
        }
        result.map_err(|error| {
            if self.has_exited() {
                return self.process_exited_error();
            }
            if is_connection_closed_error(&error) {
                return LspError::ConnectionClosed {
                    server_id: self.server.id.clone(),
                    root: self.root.clone(),
                    message: Some(error.to_string()),
                };
            }
            error
        })
    }

    pub async fn send_notification(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<(), LspError> {
        let Some(connection) = lock(&self.connection).clone() else {
            return Ok(());
        };
        if self.has_exited() {
            return Ok(());
        }
        connection
            .send_notification(method, params)
            .await
            .map_err(|error| {
                if is_connection_closed_error(&error) {
                    LspError::ConnectionClosed {
                        server_id: self.server.id.clone(),
                        root: self.root.clone(),
                        message: Some(error.to_string()),
                    }
                } else {
                    error
                }
            })
    }

    /// TS `stop`: shutdown request, exit notification, SIGTERM, then SIGKILL after
    /// `STOP_HARD_KILL_TIMEOUT_MS`.
    pub async fn stop(&self) {
        let has_connection = lock(&self.connection).is_some();
        if has_connection {
            if let Err(error) = self.send_request("shutdown", None, None, None).await {
                report_best_effort_cleanup_error("shutdown request", &error);
            }
            if let Err(error) = self.send_notification("exit", None).await {
                report_best_effort_cleanup_error("exit notification", &error);
            }
            if let Some(connection) = lock(&self.connection).take() {
                connection.dispose();
            }
        }
        let process = lock(&self.process).take();
        if let Some(mut process) = process {
            process.kill(KillSignal::Term);
            let exited = tokio::time::timeout(
                Duration::from_millis(STOP_HARD_KILL_TIMEOUT_MS),
                process.exited(),
            )
            .await
            .is_ok();
            if !exited {
                process.kill(KillSignal::Kill);
                let _ = tokio::time::timeout(
                    Duration::from_millis(STOP_SIGKILL_GRACE_MS),
                    process.exited(),
                )
                .await;
            }
        }
        self.process_exited.store(true, Ordering::SeqCst);
        lock(&self.diagnostics_store).clear();
    }

    /// TS `LspClientConnection.initialize`.
    pub async fn initialize(&self) -> Result<(), LspError> {
        let root_uri = crate::lsp::workspace_document_state::file_uri(&self.root);
        let mut workspace = json!({
            "symbol": {},
            "workspaceFolders": true,
            "configuration": true,
        });
        if self.has_workspace_apply_edit_handler() {
            workspace["applyEdit"] = json!(true);
        }
        workspace["workspaceEdit"] = json!({
            "documentChanges": true,
            "resourceOperations": ["create", "rename", "delete"],
        });
        let mut params = json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            "rootPath": self.root,
            "workspaceFolders": [{ "uri": root_uri, "name": "workspace" }],
            "capabilities": {
                "textDocument": {
                    "hover": { "contentFormat": ["markdown", "plaintext"] },
                    "definition": { "linkSupport": true },
                    "references": {},
                    "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
                    "publishDiagnostics": {},
                    "rename": { "prepareSupport": true, "prepareSupportDefaultBehavior": 1 },
                    "codeAction": {
                        "codeActionLiteralSupport": {
                            "codeActionKind": {
                                "valueSet": [
                                    "quickfix",
                                    "refactor",
                                    "refactor.extract",
                                    "refactor.inline",
                                    "refactor.rewrite",
                                    "source",
                                    "source.organizeImports",
                                    "source.fixAll",
                                ],
                            },
                        },
                        "isPreferredSupport": true,
                        "disabledSupport": true,
                        "dataSupport": true,
                        "resolveSupport": { "properties": ["edit", "command"] },
                    },
                },
                "workspace": workspace,
            },
        });
        if let Some(initialization) = &self.server.initialization {
            params["initializationOptions"] = Value::Object(initialization.clone());
        }
        let result = self
            .send_request(
                "initialize",
                Some(params),
                Some(self.initialize_timeout_ms),
                None,
            )
            .await?;
        let pull_supported = result
            .get("capabilities")
            .and_then(Value::as_object)
            .is_some_and(|capabilities| capabilities.contains_key("diagnosticProvider"));
        self.set_diagnostic_pull_supported(pull_supported);
        self.send_notification("initialized", Some(json!({})))
            .await?;
        self.send_notification(
            "workspace/didChangeConfiguration",
            Some(json!({ "settings": { "json": { "validate": { "enable": true } } } })),
        )
        .await
    }
}

async fn read_stderr(
    mut stderr: tokio::process::ChildStderr,
    buffer: Arc<Mutex<VecDeque<String>>>,
) {
    let mut chunk = vec![0_u8; 8192];
    while let Ok(read) = stderr.read(&mut chunk).await {
        if read == 0 {
            break;
        }
        let mut buffer = lock(&buffer);
        buffer.push_back(String::from_utf8_lossy(&chunk[..read]).into_owned());
        if buffer.len() > STDERR_CHUNK_LIMIT {
            buffer.pop_front();
        }
    }
}

fn is_connection_closed_error(error: &LspError) -> bool {
    let message = error.to_string().to_lowercase();
    message.contains("connection closed")
        || message.contains("connection is disposed")
        || message.contains("stream was destroyed")
}
