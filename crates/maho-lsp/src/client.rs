//! One language server: a local child process or a link through the `mhc-lsp` daemon.
use crate::config::{ResolvedServer, additional_path_bases};
use crate::error::LspError;
use crate::limits;
use crate::rpc::{Connection, RequestError};
use crate::servers::EXT_TO_LANG;
use crate::types::Diagnostic;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::sync::{mpsc, watch};

/// Waits around document sync and initialization (some servers need them before answering).
#[derive(Clone, Copy, Debug)]
pub struct ClientTimings {
    /// After spawning, before checking that the process is still running.
    pub startup_probe: Duration,
    /// After the first `didOpen` of a document.
    pub post_open: Duration,
    /// Before asking for diagnostics.
    pub post_diagnostics: Duration,
    /// After `initialized`.
    pub initialize_settle: Duration,
}

impl Default for ClientTimings {
    fn default() -> Self {
        Self {
            startup_probe: Duration::from_millis(100),
            post_open: Duration::from_millis(1000),
            post_diagnostics: Duration::from_millis(500),
            initialize_settle: Duration::from_millis(300),
        }
    }
}

impl ClientTimings {
    /// No waits (tests, in-process servers).
    pub fn immediate() -> Self {
        Self {
            startup_probe: Duration::ZERO,
            post_open: Duration::ZERO,
            post_diagnostics: Duration::ZERO,
            initialize_settle: Duration::ZERO,
        }
    }
}

/// The language id for a file extension (with dot), `plaintext` when unknown.
pub fn language_id(extension: &str) -> &'static str {
    EXT_TO_LANG
        .iter()
        .find(|(ext, _)| *ext == extension)
        .map(|(_, id)| *id)
        .unwrap_or("plaintext")
}

/// `file://` URI of an absolute path.
pub fn path_to_uri(path: &Path) -> String {
    url::Url::from_file_path(path)
        .map(String::from)
        .unwrap_or_else(|()| format!("file://{}", path.display()))
}

/// Filesystem path of a `file://` URI.
pub fn uri_to_path(uri: &str) -> PathBuf {
    url::Url::parse(uri)
        .ok()
        .and_then(|url| url.to_file_path().ok())
        .unwrap_or_else(|| PathBuf::from(uri.strip_prefix("file://").unwrap_or(uri)))
}

/// Extension with leading dot (`.rs`), or `""`.
pub fn dotted_extension(path: &Path) -> String {
    path.extension()
        .map(|ext| format!(".{}", ext.to_string_lossy()))
        .unwrap_or_default()
}

struct ProcessHandle {
    pid: Option<u32>,
    exit: watch::Receiver<Option<Option<i32>>>,
    kill: mpsc::UnboundedSender<()>,
}

impl ProcessHandle {
    fn exit_code(&self) -> Option<Option<i32>> {
        *self.exit.borrow()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Default)]
struct Document {
    version: i64,
    text: String,
}

/// A started language server.
pub struct LspClient {
    root: PathBuf,
    server: ResolvedServer,
    timings: ClientTimings,
    connection: Mutex<Option<Arc<Connection>>>,
    process: Mutex<Option<ProcessHandle>>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    documents: tokio::sync::Mutex<HashMap<String, Document>>,
    pull_errors: Mutex<Vec<String>>,
    via_daemon: Mutex<bool>,
}

impl LspClient {
    pub fn new(root: PathBuf, server: ResolvedServer, timings: ClientTimings) -> Self {
        Self {
            root,
            server,
            timings,
            connection: Mutex::new(None),
            process: Mutex::new(None),
            stderr: Arc::default(),
            documents: tokio::sync::Mutex::default(),
            pull_errors: Mutex::default(),
            via_daemon: Mutex::new(false),
        }
    }

    /// A client over an already connected peer (in-process servers, daemon links).
    pub fn with_connection(
        root: PathBuf,
        server: ResolvedServer,
        timings: ClientTimings,
        connection: Arc<Connection>,
    ) -> Self {
        let client = Self::new(root, server, timings);
        *lock(&client.connection) = Some(connection);
        client
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn server(&self) -> &ResolvedServer {
        &self.server
    }
    pub fn command(&self) -> Vec<String> {
        self.server.command.clone()
    }
    pub fn pid(&self) -> Option<u32> {
        lock(&self.process).as_ref().and_then(|process| process.pid)
    }
    /// Whether this client is linked through the daemon rather than owning its process.
    pub fn via_daemon(&self) -> bool {
        *lock(&self.via_daemon)
    }
    /// `textDocument/diagnostic` failures other than "unsupported".
    pub fn diagnostic_pull_errors(&self) -> Vec<String> {
        lock(&self.pull_errors).clone()
    }

    /// Environment the server is spawned with: ours, the server's overlay, `PATH` extended with
    /// `<root>/node_modules/.bin`.
    pub fn spawn_env(&self) -> Vec<(String, String)> {
        let base = std::env::var("PATH")
            .or_else(|_| std::env::var("Path"))
            .unwrap_or_default();
        let base = self.server.env.get("PATH").cloned().unwrap_or(base);
        let mut parts: Vec<PathBuf> = std::env::split_paths(&base).collect();
        parts.extend(additional_path_bases(&self.root));
        let path = std::env::join_paths(parts.iter().filter(|part| !part.as_os_str().is_empty()))
            .map(|joined| joined.to_string_lossy().into_owned())
            .unwrap_or(base);
        let mut env: Vec<(String, String)> = self
            .server
            .env
            .iter()
            .filter(|(key, _)| key.as_str() != "PATH")
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        env.push(("PATH".to_owned(), path));
        env
    }

    /// Spawns the server process (when not already connected).
    pub async fn start(&self) -> Result<(), LspError> {
        if lock(&self.connection).is_some() {
            return Ok(());
        }
        let Some((program, args)) = self.server.command.split_first() else {
            return Err(LspError::Spawn(format!(
                "LSP server {} has an empty command",
                self.server.id
            )));
        };
        let mut command = tokio::process::Command::new(program);
        command
            .args(args)
            .current_dir(&self.root)
            .envs(self.spawn_env())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|error| {
            LspError::Spawn(format!(
                "Failed to spawn LSP server {} ({program}): {error}",
                self.server.id
            ))
        })?;
        let (stdin, stdout, stderr) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take());
        if let Some(mut stderr) = stderr {
            let tail = Arc::clone(&self.stderr);
            tokio::spawn(async move {
                let mut chunk = vec![0; 4096];
                while let Ok(count) = stderr.read(&mut chunk).await {
                    if count == 0 {
                        break;
                    }
                    let mut tail = lock(&tail);
                    tail.push_back(String::from_utf8_lossy(&chunk[..count]).into_owned());
                    if tail.len() > 100 {
                        tail.pop_front();
                    }
                }
            });
        }
        let pid = child.id();
        let (exit_sender, exit) = watch::channel(None);
        let (kill, mut kill_requests) = mpsc::unbounded_channel::<()>();
        tokio::spawn(async move {
            let status = tokio::select! {
                status = child.wait() => status,
                _ = kill_requests.recv() => {
                    let _signal = child.start_kill();
                    child.wait().await
                }
            };
            let _sent = exit_sender.send(Some(status.ok().and_then(|status| status.code())));
        });
        *lock(&self.process) = Some(ProcessHandle { pid, exit, kill });
        if !self.timings.startup_probe.is_zero() {
            tokio::time::sleep(self.timings.startup_probe).await;
        }
        if let Some(code) = lock(&self.process)
            .as_ref()
            .and_then(ProcessHandle::exit_code)
        {
            let tail = self.stderr_tail(usize::MAX);
            let tail = tail
                .chars()
                .rev()
                .take(2000)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<String>();
            return Err(self.exited_error(code, Some(tail)));
        }
        let (Some(stdin), Some(stdout)) = (stdin, stdout) else {
            return Err(LspError::Spawn(format!(
                "LSP server {} has no stdio pipes",
                self.server.id
            )));
        };
        *lock(&self.connection) = Some(Connection::start(stdout, stdin));
        Ok(())
    }

    /// Uses a daemon link instead of spawning.
    pub fn attach_daemon_connection(&self, connection: Arc<Connection>) {
        *lock(&self.connection) = Some(connection);
        *lock(&self.via_daemon) = true;
    }

    fn stderr_tail(&self, chunks: usize) -> String {
        let tail = lock(&self.stderr);
        let skip = tail.len().saturating_sub(chunks);
        tail.iter()
            .skip(skip)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn exited_error(&self, code: Option<i32>, stderr_tail: Option<String>) -> LspError {
        LspError::ProcessExited {
            server_id: self.server.id.clone(),
            root: self.root.display().to_string(),
            exit_code: code,
            stderr_tail: stderr_tail.filter(|tail| !tail.is_empty()),
        }
    }

    fn process_exit(&self) -> Option<Option<i32>> {
        lock(&self.process)
            .as_ref()
            .and_then(ProcessHandle::exit_code)
    }

    fn connection(&self) -> Result<Arc<Connection>, LspError> {
        lock(&self.connection)
            .clone()
            .ok_or_else(|| LspError::Other("LSP client not started".to_owned()))
    }

    /// Process (if owned) running and connection open.
    pub fn is_alive(&self) -> bool {
        let connected = lock(&self.connection)
            .as_ref()
            .is_some_and(|connection| !connection.is_closed());
        connected && self.process_exit().is_none()
    }

    /// Sends a request with the server's request timeout.
    pub async fn request(&self, method: &str, params: Option<Value>) -> Result<Value, LspError> {
        let connection = self.connection()?;
        if let Some(code) = self.process_exit() {
            return Err(self.exited_error(code, Some(self.stderr_tail(10))));
        }
        let timeout = self
            .server
            .request_timeout
            .unwrap_or(limits::REQUEST_TIMEOUT);
        match connection.request(method, params, timeout).await {
            Ok(value) => Ok(value),
            Err(RequestError::Timeout) => Err(LspError::RequestTimeout {
                method: method.to_owned(),
                stderr_tail: Some(self.stderr_tail(5)).filter(|tail| !tail.is_empty()),
            }),
            Err(RequestError::Rpc(failure)) => Err(LspError::Rpc {
                code: failure.code,
                message: failure.message,
            }),
            Err(RequestError::Closed) => {
                tokio::task::yield_now().await;
                match self.process_exit() {
                    Some(code) => Err(self.exited_error(code, Some(self.stderr_tail(10)))),
                    None => Err(LspError::ConnectionClosed {
                        server_id: self.server.id.clone(),
                        root: self.root.display().to_string(),
                        message: None,
                    }),
                }
            }
        }
    }

    /// Sends a notification; silently dropped when the server is gone.
    pub fn notify(&self, method: &str, params: Option<Value>) {
        if let Ok(connection) = self.connection()
            && self.process_exit().is_none()
        {
            connection.notify(method, params);
        }
    }

    /// `initialize` + `initialized` + the JSON validation settings push.
    pub async fn initialize(&self) -> Result<(), LspError> {
        let root_uri = path_to_uri(&self.root);
        let mut params = json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            "rootPath": self.root.display().to_string(),
            "workspaceFolders": [{"uri": root_uri, "name": "workspace"}],
            "capabilities": {
                "textDocument": {
                    "hover": {"contentFormat": ["markdown", "plaintext"]},
                    "definition": {"linkSupport": true},
                    "references": {},
                    "documentSymbol": {"hierarchicalDocumentSymbolSupport": true},
                    "publishDiagnostics": {},
                    "rename": {"prepareSupport": true, "prepareSupportDefaultBehavior": 1, "honorsChangeAnnotations": true},
                    "codeAction": {
                        "codeActionLiteralSupport": {"codeActionKind": {"valueSet": [
                            "quickfix", "refactor", "refactor.extract", "refactor.inline", "refactor.rewrite",
                            "source", "source.organizeImports", "source.fixAll"
                        ]}},
                        "isPreferredSupport": true,
                        "disabledSupport": true,
                        "dataSupport": true,
                        "resolveSupport": {"properties": ["edit", "command"]}
                    }
                },
                "workspace": {
                    "symbol": {},
                    "workspaceFolders": true,
                    "configuration": true,
                    "applyEdit": true,
                    "workspaceEdit": {"documentChanges": true}
                }
            },
        });
        if let Some(options) = self.server.initialization.clone() {
            params["initializationOptions"] = options;
        }
        self.request("initialize", Some(params)).await?;
        self.notify("initialized", Some(json!({})));
        self.notify(
            "workspace/didChangeConfiguration",
            Some(json!({"settings": {"json": {"validate": {"enable": true}}}})),
        );
        if !self.timings.initialize_settle.is_zero() {
            tokio::time::sleep(self.timings.initialize_settle).await;
        }
        Ok(())
    }

    /// Opens the document, or syncs its current text when it changed since the last sync.
    pub async fn open_file(&self, path: &Path) -> Result<String, LspError> {
        let path = absolute(path);
        let uri = path_to_uri(&path);
        let text = std::fs::read_to_string(&path)
            .map_err(|error| LspError::Other(format!("{}: {error}", path.display())))?;
        let mut documents = self.documents.lock().await;
        match documents.get_mut(&uri) {
            None => {
                self.notify(
                    "textDocument/didOpen",
                    Some(json!({"textDocument": {"uri": uri, "languageId": language_id(&dotted_extension(&path)), "version": 1, "text": text}})),
                );
                documents.insert(uri.clone(), Document { version: 1, text });
                drop(documents);
                if !self.timings.post_open.is_zero() {
                    tokio::time::sleep(self.timings.post_open).await;
                }
            }
            Some(document) if document.text == text => {}
            Some(document) => {
                document.version += 1;
                document.text.clone_from(&text);
                let version = document.version;
                self.notify("textDocument/didChange", Some(json!({"textDocument": {"uri": uri, "version": version}, "contentChanges": [{"text": text}]})));
                self.notify(
                    "textDocument/didSave",
                    Some(json!({"textDocument": {"uri": uri}, "text": text})),
                );
            }
        }
        Ok(uri)
    }

    async fn position_request(
        &self,
        method: &str,
        path: &Path,
        line: u32,
        character: u32,
        extra: Value,
    ) -> Result<Value, LspError> {
        let uri = self.open_file(path).await?;
        let mut params = json!({"textDocument": {"uri": uri}, "position": {"line": line.saturating_sub(1), "character": character}});
        if let (Some(target), Some(extra)) = (params.as_object_mut(), extra.as_object()) {
            target.extend(extra.clone());
        }
        self.request(method, Some(params)).await
    }

    /// `textDocument/definition` (1-based line, 0-based character).
    pub async fn definition(
        &self,
        path: &Path,
        line: u32,
        character: u32,
    ) -> Result<Value, LspError> {
        self.position_request("textDocument/definition", path, line, character, json!({}))
            .await
    }

    pub async fn references(
        &self,
        path: &Path,
        line: u32,
        character: u32,
        include_declaration: bool,
    ) -> Result<Value, LspError> {
        self.position_request(
            "textDocument/references",
            path,
            line,
            character,
            json!({"context": {"includeDeclaration": include_declaration}}),
        )
        .await
    }

    pub async fn document_symbols(&self, path: &Path) -> Result<Value, LspError> {
        let uri = self.open_file(path).await?;
        self.request(
            "textDocument/documentSymbol",
            Some(json!({"textDocument": {"uri": uri}})),
        )
        .await
    }

    pub async fn workspace_symbols(&self, query: &str) -> Result<Value, LspError> {
        self.request("workspace/symbol", Some(json!({"query": query})))
            .await
    }

    pub async fn prepare_rename(
        &self,
        path: &Path,
        line: u32,
        character: u32,
    ) -> Result<Value, LspError> {
        self.position_request(
            "textDocument/prepareRename",
            path,
            line,
            character,
            json!({}),
        )
        .await
    }

    pub async fn rename(
        &self,
        path: &Path,
        line: u32,
        character: u32,
        new_name: &str,
    ) -> Result<Value, LspError> {
        self.position_request(
            "textDocument/rename",
            path,
            line,
            character,
            json!({"newName": new_name}),
        )
        .await
    }

    /// Pull diagnostics (`textDocument/diagnostic`), falling back to the last published ones.
    pub async fn diagnostics(&self, path: &Path) -> Result<Vec<Diagnostic>, LspError> {
        let uri = self.open_file(path).await?;
        if !self.timings.post_diagnostics.is_zero() {
            tokio::time::sleep(self.timings.post_diagnostics).await;
        }
        match self
            .request(
                "textDocument/diagnostic",
                Some(json!({"textDocument": {"uri": uri}})),
            )
            .await
        {
            Ok(result) => {
                if let Some(items) = result
                    .get("items")
                    .and_then(|items| serde_json::from_value::<Vec<Diagnostic>>(items.clone()).ok())
                {
                    return Ok(items);
                }
            }
            Err(error) => {
                if error.is_dead_connection() {
                    return Err(error);
                }
                if !is_unsupported_pull(&error) {
                    lock(&self.pull_errors).push(error.to_string());
                }
            }
        }
        Ok(self.connection()?.stored_diagnostics(&uri))
    }

    /// `shutdown` + `exit`, then terminate the owned process (SIGKILL after the grace period).
    pub async fn stop(&self) {
        let connection = lock(&self.connection).take();
        if let Some(connection) = connection {
            if !connection.is_closed() && self.process_exit().is_none() {
                let _ignored = connection
                    .request("shutdown", None, Duration::from_secs(2))
                    .await;
                connection.notify("exit", None);
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            connection.close();
            connection.clear_diagnostics();
        }
        let process = lock(&self.process).take();
        if let Some(mut process) = process {
            let exited = matches!(
                tokio::time::timeout(
                    limits::STOP_HARD_KILL_TIMEOUT,
                    process.exit.wait_for(Option::is_some)
                )
                .await,
                Ok(Ok(_))
            );
            if !exited {
                let _requested = process.kill.send(());
                let _waited = matches!(
                    tokio::time::timeout(
                        Duration::from_secs(1),
                        process.exit.wait_for(Option::is_some)
                    )
                    .await,
                    Ok(Ok(_))
                );
            }
        }
        self.documents.lock().await.clear();
    }
}

fn is_unsupported_pull(error: &LspError) -> bool {
    if let LspError::Rpc { code: -32601, .. } = error {
        return true;
    }
    let lower = error.to_string().to_lowercase();
    [
        "unsupported",
        "not supported",
        "method not found",
        "unknown request",
        "unhandled method",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// Absolute form of `path` against the process cwd.
pub fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}
