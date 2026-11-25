//! Port of `packages/machine-ops/src/types.ts`.

use crate::errors::MachineOpError;
use crate::policy::filesystem::FilesystemPolicyChecker;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

pub type OpFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, MachineOpError>> + Send + 'a>>;

/// Cloneable abort subscription; receivers observe cancellation even when registered afterwards.
#[derive(Clone, Debug)]
pub struct AbortSignal(tokio::sync::watch::Sender<Option<Option<String>>>);

impl Default for AbortSignal {
    fn default() -> Self {
        Self(tokio::sync::watch::channel(None).0)
    }
}

impl AbortSignal {
    pub fn abort(&self) {
        self.abort_with_reason(None);
    }

    pub fn abort_with_reason(&self, reason: Option<String>) {
        self.0.send_if_modified(|state| {
            if state.is_some() {
                return false;
            }
            *state = Some(reason);
            true
        });
    }

    pub fn reason(&self) -> Option<String> {
        self.0.borrow().as_ref().cloned().flatten()
    }

    pub fn is_aborted(&self) -> bool {
        self.0.borrow().is_some()
    }

    pub async fn cancelled(&self) {
        let mut receiver = self.0.subscribe();
        loop {
            if receiver.borrow_and_update().is_some() {
                return;
            }
            if receiver.changed().await.is_err() {
                return;
            }
        }
    }
}

/// Every operation name a `PolicyHooks.beforeOp` gate can see.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OpName {
    #[serde(rename = "files.read")]
    FilesRead,
    #[serde(rename = "files.write")]
    FilesWrite,
    #[serde(rename = "files.edit")]
    FilesEdit,
    #[serde(rename = "files.glob")]
    FilesGlob,
    #[serde(rename = "files.grep")]
    FilesGrep,
    #[serde(rename = "files.stat")]
    FilesStat,
    #[serde(rename = "files.list")]
    FilesList,
    #[serde(rename = "process.exec")]
    ProcessExec,
    #[serde(rename = "process.spawn")]
    ProcessSpawn,
    #[serde(rename = "process.write")]
    ProcessWrite,
    #[serde(rename = "process.kill")]
    ProcessKill,
    #[serde(rename = "process.sweep")]
    ProcessSweep,
    #[serde(rename = "info.message")]
    InfoMessage,
}

impl OpName {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FilesRead => "files.read",
            Self::FilesWrite => "files.write",
            Self::FilesEdit => "files.edit",
            Self::FilesGlob => "files.glob",
            Self::FilesGrep => "files.grep",
            Self::FilesStat => "files.stat",
            Self::FilesList => "files.list",
            Self::ProcessExec => "process.exec",
            Self::ProcessSpawn => "process.spawn",
            Self::ProcessWrite => "process.write",
            Self::ProcessKill => "process.kill",
            Self::ProcessSweep => "process.sweep",
            Self::InfoMessage => "info.message",
        }
    }
}

impl std::fmt::Display for OpName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PolicyDecision {
    Allow,
    Deny { reason: String },
}

#[derive(Clone, Debug)]
pub enum OpOutcome {
    Ok,
    Err(MachineOpError),
}

pub type BeforeOpHook = Arc<
    dyn Fn(
            OpName,
            &serde_json::Value,
            &OpContext,
        ) -> Pin<Box<dyn Future<Output = Result<PolicyDecision, MachineOpError>> + Send>>
        + Send
        + Sync,
>;

pub type AfterOpHook = Arc<
    dyn Fn(
            OpName,
            &serde_json::Value,
            OpOutcome,
            &OpContext,
        ) -> Pin<Box<dyn Future<Output = Result<(), MachineOpError>> + Send>>
        + Send
        + Sync,
>;

#[derive(Clone, Default)]
pub struct PolicyHooks {
    /// Deny-wins path checker (the filesystem policy composed from registered policies).
    pub filesystem: Option<FilesystemPolicyChecker>,
    /// The one gate both paths call before any effect: may `ctx.principal` run `op` with `args` under `ctx.scope`?
    pub before_op: Option<BeforeOpHook>,
    pub after_op: Option<AfterOpHook>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Largest file `files.read`/`files.edit`/`files.grep` reads; `Some(10 MiB)` on the machine path, `None` when unbounded.
    pub read_max_bytes: Option<usize>,
    /// Captured stdout of one `process.exec`; 8 MiB on the machine path. Streaming via `on_output` is never capped.
    pub exec_stdout_max_bytes: usize,
    /// Captured stderr of one `process.exec`; 1 MiB on the machine path.
    pub exec_stderr_max_bytes: usize,
    /// Timeout when an exec request names none; `None` disables the default timeout.
    pub exec_timeout_default_ms: Option<u64>,
    /// Screen history kept per PTY handle.
    pub pty_scrollback_bytes: usize,
    /// Raw output tail kept per PTY handle.
    pub retained_output_bytes: usize,
    /// Live PTY handles allowed at once; `None` = no cap.
    pub max_pty_sessions: Option<usize>,
}

impl Default for Limits {
    fn default() -> Self {
        crate::limits::DEFAULT_LIMITS
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PrincipalKind {
    LocalSession,
    RemoteDevice,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpPrincipal {
    pub kind: PrincipalKind,
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpScope {
    pub id: String,
    pub root: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuditVerdict {
    Allowed,
    Denied,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditEntry {
    pub op: OpName,
    pub principal: OpPrincipal,
    pub target: String,
    pub verdict: AuditVerdict,
}

pub type AuditSink = Arc<dyn Fn(AuditEntry) + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathLockMode {
    Caller,
}

#[derive(Clone)]
pub struct OpContext {
    /// `root`, when set, contains every path the operation touches (a path or symlink resolving outside it is a policy denial).
    pub scope: OpScope,
    pub principal: OpPrincipal,
    pub signal: AbortSignal,
    pub limits: Limits,
    pub policy: PolicyHooks,
    pub audit: Option<AuditSink>,
    /// Set by a caller that already holds the per-path mutation slot of every path this operation mutates.
    pub path_lock: Option<PathLockMode>,
}

#[derive(Clone, Debug)]
pub struct ReadRequest {
    pub path: String,
    pub offset: Option<u64>,
    pub limit_bytes: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadResult {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
    /// Size of the whole file.
    pub size: u64,
    pub offset: u64,
}

#[derive(Clone, Debug)]
pub struct WriteRequest {
    pub path: String,
    pub bytes: Vec<u8>,
    pub create_parent_dirs: bool,
    /// Write `bytes` at this byte offset of an existing or new file instead of replacing the file.
    pub offset: Option<u64>,
    /// With `offset`: the final file size, so ranges written in any order leave the same file.
    pub total: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteResult {
    pub path: PathBuf,
    pub written_bytes: usize,
}

#[derive(Clone, Debug)]
pub struct EditRequest {
    pub path: String,
    pub exact: String,
    pub replacement: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditResult {
    pub path: PathBuf,
    pub content: String,
    pub replacements: usize,
    pub write_result: WriteResult,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryType {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Clone, Debug)]
pub struct GlobRequest {
    pub pattern: String,
    pub cwd: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlobEntry {
    pub path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlobResult {
    pub entries: Vec<GlobEntry>,
    pub truncated: bool,
}

#[derive(Clone, Debug)]
pub struct GrepRequest {
    pub path: String,
    pub pattern: String,
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrepMatch {
    pub path: PathBuf,
    pub line: usize,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrepResult {
    pub matches: Vec<GrepMatch>,
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatAccess {
    Read,
    Write,
    ReadWrite,
}

#[derive(Clone, Debug)]
pub struct StatRequest {
    pub path: String,
    pub access: Option<StatAccess>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatResult {
    pub path: PathBuf,
    pub entry_type: EntryType,
    pub size: u64,
    pub mtime_ms: u64,
    pub mode: u32,
}

#[derive(Clone, Debug)]
pub struct ListRequest {
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListEntry {
    pub name: String,
    pub entry_type: EntryType,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListResult {
    pub path: PathBuf,
    pub entries: Vec<ListEntry>,
}

pub trait FileOps: Send + Sync {
    fn read<'a>(&'a self, req: ReadRequest, ctx: &'a OpContext) -> OpFuture<'a, ReadResult>;
    fn write<'a>(&'a self, req: WriteRequest, ctx: &'a OpContext) -> OpFuture<'a, WriteResult>;
    fn edit<'a>(&'a self, req: EditRequest, ctx: &'a OpContext) -> OpFuture<'a, EditResult>;
    fn glob<'a>(&'a self, req: GlobRequest, ctx: &'a OpContext) -> OpFuture<'a, GlobResult>;
    fn grep<'a>(&'a self, req: GrepRequest, ctx: &'a OpContext) -> OpFuture<'a, GrepResult>;
    fn stat<'a>(&'a self, req: StatRequest, ctx: &'a OpContext) -> OpFuture<'a, StatResult>;
    fn list<'a>(&'a self, req: ListRequest, ctx: &'a OpContext) -> OpFuture<'a, ListResult>;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CommandTransport {
    #[default]
    Argv,
    Stdin,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecShell {
    pub path: String,
    pub args: Vec<String>,
    pub command_transport: CommandTransport,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EnvMode {
    #[default]
    Overlay,
    Exact,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecStream {
    Stdout,
    Stderr,
}

pub type ExecOutputCallback =
    Arc<dyn Fn(&[u8], ExecStream) -> Result<(), MachineOpError> + Send + Sync>;
pub type ExecPidCallback = Arc<dyn Fn(u32) + Send + Sync>;

#[derive(Clone, Default)]
pub struct ExecRequest {
    pub command: String,
    pub cwd: Option<PathBuf>,
    /// Environment entries; with `EnvMode::Overlay` (default) laid over a curated base, with `Exact` used as-is.
    pub env: Option<BTreeMap<String, String>>,
    pub env_mode: EnvMode,
    /// Default: `/bin/sh -c` (POSIX), `cmd.exe /d /s /c` (Windows).
    pub shell: Option<ExecShell>,
    /// Default `ctx.limits.exec_timeout_default_ms`; `Some(None)` disables the timeout.
    pub timeout_ms: Option<Option<u64>>,
    pub on_output: Option<ExecOutputCallback>,
    pub on_spawn: Option<ExecPidCallback>,
    pub on_exit: Option<ExecPidCallback>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecResult {
    pub pid: Option<u32>,
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub timed_out: bool,
    pub aborted: bool,
    pub started_at_ms: u64,
    pub duration_ms: u64,
    pub scope_token: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct SpawnRequest {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: Option<BTreeMap<String, String>>,
    pub cols: Option<u16>,
    pub rows: Option<u16>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandleLifecycle {
    Running,
    Exited,
    Killed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandleStatus {
    pub id: String,
    pub pid: u32,
    pub state: HandleLifecycle,
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    pub last_seq: u64,
    pub retained_bytes: usize,
    pub omitted_bytes: usize,
    pub started_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputChunk {
    pub seq: u64,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StdinEffect {
    pub accepted_bytes: usize,
    pub observed: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct WaitSpec {
    pub output: Option<String>,
    pub timeout_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitReason {
    Exited,
    Matched,
    Timeout,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaitResult {
    pub status: HandleStatus,
    pub reason: WaitReason,
}

pub trait ProcessHandle: Send + Sync {
    fn id(&self) -> &str;
    fn pid(&self) -> u32;
    fn scope_token(&self) -> &str;
    fn status(&self) -> HandleStatus;
    fn subscribe_output(&self, from_seq: u64) -> tokio::sync::mpsc::UnboundedReceiver<OutputChunk>;
    fn screen(&self, history: Option<usize>) -> OpFuture<'_, String>;
    fn write<'a>(
        &'a self,
        bytes: &'a [u8],
        expect_activity_ms: Option<u64>,
    ) -> OpFuture<'a, StdinEffect>;
    fn wait(&self, spec: WaitSpec) -> OpFuture<'_, WaitResult>;
    fn resize(&self, cols: u16, rows: u16);
    fn kill(&self, signal: Option<&str>) -> OpFuture<'_, ()>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandleInfo {
    pub id: String,
    pub pid: u32,
    pub principal: String,
    pub scope_token: String,
    pub command: String,
    pub state: HandleLifecycle,
}

pub trait ProcessOps: Send + Sync {
    fn exec<'a>(&'a self, req: ExecRequest, ctx: &'a OpContext) -> OpFuture<'a, ExecResult>;
    fn spawn<'a>(
        &'a self,
        req: SpawnRequest,
        ctx: &'a OpContext,
    ) -> OpFuture<'a, Arc<dyn ProcessHandle>>;
    fn handle(&self, id: &str) -> Option<Arc<dyn ProcessHandle>>;
    fn list(&self, principal: Option<&str>) -> Vec<HandleInfo>;
    fn sweep<'a>(&'a self, tokens: &'a [String]) -> OpFuture<'a, ()>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessInfo {
    pub pid: u32,
    pub started_at_ms: u64,
    pub cpu_time_ms: u64,
    pub rss_bytes: u64,
    pub command: String,
    pub last_op_at_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HostFacts {
    /// Node-compatible platform identifier (`"linux"`, `"darwin"`, `"win32"`, `"freebsd"`, ...).
    pub os: String,
    pub arch: String,
    pub load1: f64,
    pub cores: usize,
    pub runtime: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageResult {
    pub accepted: bool,
    pub seq: usize,
    pub inbox_path: PathBuf,
}

#[derive(Clone, Debug, Default)]
pub struct ProcessInfoRequest {
    pub pid: Option<u32>,
    pub handle_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct MessageRequest {
    pub topic: String,
    pub body: serde_json::Value,
}

pub trait InfoOps: Send + Sync {
    fn process_info(&self, req: ProcessInfoRequest) -> OpFuture<'_, ProcessInfo>;
    fn host(&self) -> HostFacts;
    fn message<'a>(
        &'a self,
        req: MessageRequest,
        ctx: &'a OpContext,
    ) -> OpFuture<'a, MessageResult>;
}

pub trait MachineOps: Send + Sync {
    fn files(&self) -> &dyn FileOps;
    fn process(&self) -> &dyn ProcessOps;
    fn info(&self) -> &dyn InfoOps;
    fn limits(&self) -> Limits;
}
