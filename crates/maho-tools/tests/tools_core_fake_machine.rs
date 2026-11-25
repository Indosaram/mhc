use maho_machine_ops::{
    EditRequest, EditResult, EntryType, ExecRequest, ExecResult, FileOps, GlobRequest, GlobResult,
    GrepRequest, GrepResult, HandleInfo, HostFacts, InfoOps, Limits, ListRequest, ListResult,
    MachineOpError, MachineOps, MessageRequest, MessageResult, OpContext, OpErrorCode, OpFuture,
    ProcessHandle, ProcessInfo, ProcessInfoRequest, ProcessOps, ReadRequest, ReadResult,
    SpawnRequest, StatAccess, StatRequest, StatResult, WriteRequest, WriteResult,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct ScriptedChunk {
    pub stream: maho_machine_ops::ExecStream,
    pub text: String,
}

#[derive(Clone)]
pub struct ScriptedExec {
    pub output: Vec<ScriptedChunk>,
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ErrorMode {
    Fs,
    Op,
}

pub type WriteOverride =
    Arc<dyn Fn(WriteRequest) -> Result<WriteResult, MachineOpError> + Send + Sync>;

pub struct FakeMachine {
    scripts: HashMap<String, ScriptedExec>,
    error_mode: ErrorMode,
    os: String,
    pub calls: Mutex<Vec<String>>,
    write_override: Option<WriteOverride>,
}

impl FakeMachine {
    pub fn new(scripts: HashMap<String, ScriptedExec>, error_mode: ErrorMode, os: &str) -> Self {
        Self {
            scripts,
            error_mode,
            os: os.to_owned(),
            calls: Mutex::new(Vec::new()),
            write_override: None,
        }
    }

    pub fn with_write_override<F>(mut self, f: F) -> Self
    where
        F: Fn(WriteRequest) -> Result<WriteResult, MachineOpError> + Send + Sync + 'static,
    {
        self.write_override = Some(Arc::new(f));
        self
    }

    pub fn call_log(&self) -> Vec<String> {
        self.calls.lock().expect("calls lock").clone()
    }

    fn record(&self, op: &str) {
        self.calls.lock().expect("calls lock").push(op.to_owned());
    }

    fn map_io_err(
        &self,
        err: std::io::Error,
        syscall: &str,
        path: &std::path::Path,
    ) -> MachineOpError {
        if self.error_mode == ErrorMode::Op && err.kind() == std::io::ErrorKind::NotFound {
            return MachineOpError::new(OpErrorCode::NotFound, format!("not found: {err}"));
        }
        MachineOpError::from_io(&err, Some(syscall), Some(path))
    }
}

impl MachineOps for FakeMachine {
    fn files(&self) -> &dyn FileOps {
        self
    }

    fn process(&self) -> &dyn ProcessOps {
        self
    }

    fn info(&self) -> &dyn InfoOps {
        self
    }

    fn limits(&self) -> Limits {
        Limits::default()
    }
}

impl FileOps for FakeMachine {
    fn read<'a>(&'a self, req: ReadRequest, _ctx: &'a OpContext) -> OpFuture<'a, ReadResult> {
        Box::pin(async move {
            self.record("files.read");
            let path = PathBuf::from(&req.path);
            let whole = tokio::fs::read(&path)
                .await
                .map_err(|e| self.map_io_err(e, "open", &path))?;
            let offset = req.offset.unwrap_or(0) as usize;
            let start = offset.min(whole.len());
            let end = match req.limit_bytes {
                Some(limit) => start.saturating_add(limit).min(whole.len()),
                None => whole.len(),
            };
            Ok(ReadResult {
                path,
                bytes: whole[start..end].to_vec(),
                size: whole.len() as u64,
                offset: offset as u64,
            })
        })
    }

    fn write<'a>(&'a self, req: WriteRequest, _ctx: &'a OpContext) -> OpFuture<'a, WriteResult> {
        Box::pin(async move {
            self.record("files.write");
            if let Some(override_fn) = &self.write_override {
                return override_fn(req);
            }
            let path = PathBuf::from(&req.path);
            if req.create_parent_dirs
                && let Some(parent) = path.parent()
            {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| self.map_io_err(e, "mkdir", parent))?;
            }
            if let Some(offset) = req.offset {
                let offset_usize = offset as usize;
                let mut existing = tokio::fs::read(&path).await.unwrap_or_default();
                let target_len = existing.len().max(offset_usize + req.bytes.len());
                if existing.len() < target_len {
                    existing.resize(target_len, 0);
                }
                existing[offset_usize..offset_usize + req.bytes.len()].copy_from_slice(&req.bytes);
                tokio::fs::write(&path, &existing)
                    .await
                    .map_err(|e| self.map_io_err(e, "open", &path))?;
            } else {
                tokio::fs::write(&path, &req.bytes)
                    .await
                    .map_err(|e| self.map_io_err(e, "open", &path))?;
            }
            Ok(WriteResult {
                path,
                written_bytes: req.bytes.len(),
            })
        })
    }

    fn edit<'a>(&'a self, _req: EditRequest, _ctx: &'a OpContext) -> OpFuture<'a, EditResult> {
        Box::pin(async move {
            Err(MachineOpError::new(
                OpErrorCode::IoError,
                "files.edit is not part of the parity scenarios",
            ))
        })
    }

    fn glob<'a>(&'a self, _req: GlobRequest, _ctx: &'a OpContext) -> OpFuture<'a, GlobResult> {
        Box::pin(async move {
            Err(MachineOpError::new(
                OpErrorCode::IoError,
                "files.glob is not part of the parity scenarios",
            ))
        })
    }

    fn grep<'a>(&'a self, _req: GrepRequest, _ctx: &'a OpContext) -> OpFuture<'a, GrepResult> {
        Box::pin(async move {
            Err(MachineOpError::new(
                OpErrorCode::IoError,
                "files.grep is not part of the parity scenarios",
            ))
        })
    }

    fn stat<'a>(&'a self, req: StatRequest, _ctx: &'a OpContext) -> OpFuture<'a, StatResult> {
        Box::pin(async move {
            self.record("files.stat");
            let path = PathBuf::from(&req.path);
            let meta = tokio::fs::metadata(&path)
                .await
                .map_err(|e| self.map_io_err(e, "stat", &path))?;
            if let Some(access_mode) = req.access {
                #[cfg(unix)]
                {
                    use nix::unistd::{AccessFlags, access};
                    let flags = match access_mode {
                        StatAccess::Read => AccessFlags::R_OK,
                        StatAccess::Write => AccessFlags::W_OK,
                        StatAccess::ReadWrite => AccessFlags::R_OK | AccessFlags::W_OK,
                    };
                    access(&path, flags).map_err(|errno| {
                        let io_err = std::io::Error::from_raw_os_error(errno as i32);
                        self.map_io_err(io_err, "access", &path)
                    })?;
                }
                #[cfg(not(unix))]
                {
                    let _ = access_mode;
                }
            }
            let entry_type = if meta.is_dir() {
                EntryType::Directory
            } else if meta.is_file() {
                EntryType::File
            } else {
                EntryType::Other
            };
            Ok(StatResult {
                path,
                entry_type,
                size: meta.len(),
                mtime_ms: 0,
                mode: 0o644,
            })
        })
    }

    fn list<'a>(&'a self, _req: ListRequest, _ctx: &'a OpContext) -> OpFuture<'a, ListResult> {
        Box::pin(async move {
            Err(MachineOpError::new(
                OpErrorCode::IoError,
                "files.list is not part of the parity scenarios",
            ))
        })
    }
}

impl ProcessOps for FakeMachine {
    fn exec<'a>(&'a self, req: ExecRequest, _ctx: &'a OpContext) -> OpFuture<'a, ExecResult> {
        Box::pin(async move {
            self.record("process.exec");
            let script = self.scripts.get(&req.command).ok_or_else(|| {
                MachineOpError::new(
                    OpErrorCode::InvalidArgument,
                    format!("no scripted answer for {}", req.command),
                )
            })?;
            if let Some(on_output) = &req.on_output {
                for chunk in &script.output {
                    on_output(chunk.text.as_bytes(), chunk.stream)?;
                }
            }
            Ok(ExecResult {
                pid: None,
                exit_code: script.exit_code,
                signal: script.signal.clone(),
                stdout: Vec::new(),
                stderr: Vec::new(),
                stdout_truncated: false,
                stderr_truncated: false,
                timed_out: false,
                aborted: false,
                started_at_ms: 0,
                duration_ms: 0,
                scope_token: None,
            })
        })
    }

    fn spawn<'a>(
        &'a self,
        _req: SpawnRequest,
        _ctx: &'a OpContext,
    ) -> OpFuture<'a, Arc<dyn ProcessHandle>> {
        Box::pin(async move {
            Err(MachineOpError::new(
                OpErrorCode::IoError,
                "process.spawn is not part of the parity scenarios",
            ))
        })
    }

    fn handle(&self, _id: &str) -> Option<Arc<dyn ProcessHandle>> {
        None
    }

    fn list(&self, _principal: Option<&str>) -> Vec<HandleInfo> {
        Vec::new()
    }

    fn sweep<'a>(&'a self, _tokens: &'a [String]) -> OpFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}

impl InfoOps for FakeMachine {
    fn process_info(&self, _req: ProcessInfoRequest) -> OpFuture<'_, ProcessInfo> {
        Box::pin(async move {
            Err(MachineOpError::new(
                OpErrorCode::IoError,
                "info.processInfo is not part of the parity scenarios",
            ))
        })
    }

    fn host(&self) -> HostFacts {
        HostFacts {
            os: self.os.clone(),
            arch: std::env::consts::ARCH.to_owned(),
            load1: 0.0,
            cores: 1,
            runtime: "parity-fake".to_owned(),
        }
    }

    fn message<'a>(
        &'a self,
        _req: MessageRequest,
        _ctx: &'a OpContext,
    ) -> OpFuture<'a, MessageResult> {
        Box::pin(async move {
            Err(MachineOpError::new(
                OpErrorCode::IoError,
                "info.message is not part of the parity scenarios",
            ))
        })
    }
}
