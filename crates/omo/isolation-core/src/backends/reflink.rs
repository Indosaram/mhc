use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::backend::{
    BackendKind, IsolationBackend, IsolationContext, IsolationError, ProbeResult, Result,
    StartDetail,
};
use crate::backend_marker::mark_started;
use crate::backends::copy_tree::{copy_budget, copy_tree};
use crate::backends::runtime::{BackendRuntime, SystemRuntime};
use crate::git::command::exists;
use crate::util::{dirname, lock, mkdtemp_in};

pub const FICLONE: u64 = 0x40049409;

pub trait ReflinkIoctl: Send + Sync {
    fn ioctl(&self, dst: i32, request: u64, src: i32) -> i32;
    fn errno(&self) -> i32;
}

pub type LoadReflink = Arc<dyn Fn() -> Option<Arc<dyn ReflinkIoctl>> + Send + Sync>;

#[cfg(target_os = "linux")]
struct LibcReflink;

#[cfg(target_os = "linux")]
impl ReflinkIoctl for LibcReflink {
    fn ioctl(&self, dst: i32, request: u64, src: i32) -> i32 {
        unsafe { libc::ioctl(dst, request as libc::c_ulong, src as libc::c_int) }
    }

    fn errno(&self) -> i32 {
        std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
    }
}

pub fn default_load_reflink() -> Option<Arc<dyn ReflinkIoctl>> {
    #[cfg(target_os = "linux")]
    {
        Some(Arc::new(LibcReflink))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[derive(Default)]
struct ReflinkState {
    loaded: bool,
    ffi: Option<Arc<dyn ReflinkIoctl>>,
    unavailable_reason: Option<String>,
}

pub struct ReflinkBackend {
    io: Arc<dyn BackendRuntime>,
    load: LoadReflink,
    state: Mutex<ReflinkState>,
}

impl Default for ReflinkBackend {
    fn default() -> Self {
        ReflinkBackend {
            io: Arc::new(SystemRuntime),
            load: Arc::new(default_load_reflink),
            state: Mutex::new(ReflinkState::default()),
        }
    }
}

impl ReflinkBackend {
    pub fn new(io: Arc<dyn BackendRuntime>, load: LoadReflink) -> Self {
        ReflinkBackend {
            io,
            load,
            state: Mutex::new(ReflinkState::default()),
        }
    }

    fn initialize(&self) {
        let mut state = lock(&self.state);
        if !state.loaded {
            state.ffi = (self.load)();
            state.loaded = true;
        }
    }

    fn clone_file(
        &self,
        ffi: &Arc<dyn ReflinkIoctl>,
        source: &Path,
        destination: &Path,
    ) -> Result<()> {
        let src = std::fs::File::open(source)?;
        let dst = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)?;
        if ffi.ioctl(dst.as_raw_fd(), FICLONE, src.as_raw_fd()) != 0 {
            let errno = ffi.errno();
            if [18, 95, 9, 25, 38].contains(&errno) {
                return Err(IsolationError::unavailable(format!(
                    "FICLONE unavailable: errno {errno}"
                )));
            }
            return Err(IsolationError::other(format!("FICLONE failed: errno {errno}")));
        }
        Ok(())
    }

    fn cp(&self, source: &Path, destination: &Path) -> Result<()> {
        let result = self.io.run(&[
            "cp".to_string(),
            "-a".to_string(),
            "--reflink=always".to_string(),
            source.to_string_lossy().into_owned(),
            destination.to_string_lossy().into_owned(),
        ])?;
        if result.code == 0 {
            return Ok(());
        }
        if result.code == 1 && is_cp_unsupported(&result.stderr) {
            return Err(IsolationError::unavailable(result.stderr));
        }
        Err(IsolationError::other(format!(
            "cp exited {}: {}",
            result.code, result.stderr
        )))
    }

    fn probe_inner(&self, lower: &Path, ctx: Option<&IsolationContext>) -> Result<ProbeResult> {
        if self.io.platform() != "linux" {
            return Ok(ProbeResult::unavailable("FICLONE requires Linux"));
        }
        let Some(ctx) = ctx else {
            return Ok(ProbeResult::unavailable(
                "reflink probing requires an isolation context to write inside",
            ));
        };
        // Claim the supplied context: probe files live exactly inside the caller's
        // base directory, and a context we had to create is released again so the
        // exclusive publication in ensure is unaffected.
        let target = ctx.base_dir.clone();
        let existed = exists(&target)?;
        std::fs::create_dir_all(&target)?;
        let result = self.probe_inside(lower, ctx, &target);
        if !existed {
            let _ = std::fs::remove_dir(&target);
        }
        result
    }

    fn probe_inside(
        &self,
        lower: &Path,
        ctx: &IsolationContext,
        target: &Path,
    ) -> Result<ProbeResult> {
        if ctx.cross_device || self.io.device(lower)? != self.io.device(target)? {
            return Ok(ProbeResult::unavailable("reflink requires the same device"));
        }
        self.initialize();
        let ffi = lock(&self.state).ffi.clone();
        if ffi.is_none() && !self.io.which("cp") {
            lock(&self.state).unavailable_reason = Some("no FICLONE or cp".to_string());
            return Ok(ProbeResult::unavailable("no FICLONE or cp"));
        }
        let base = mkdtemp_in(target, ".omo-reflink-probe-")?;
        let outcome = (|| -> Result<ProbeResult> {
            std::fs::write(base.join("source"), "x")?;
            match &ffi {
                Some(ffi) => self.clone_file(ffi, &base.join("source"), &base.join("clone"))?,
                None => self.cp(&base.join("source"), &base.join("clone"))?,
            }
            Ok(ProbeResult::available())
        })();
        let _ = std::fs::remove_dir_all(&base);
        match outcome {
            Ok(result) => {
                lock(&self.state).unavailable_reason = None;
                Ok(result)
            }
            Err(error) if error.is_unavailable() => {
                lock(&self.state).unavailable_reason = Some(error.to_string());
                Ok(ProbeResult::unavailable(error.to_string()))
            }
            Err(error) => Err(error),
        }
    }
}

fn is_cp_unsupported(stderr: &str) -> bool {
    let lower = stderr.to_lowercase();
    lower.contains("failed to clone")
        || lower.contains("operation not supported")
        || lower.contains("invalid cross-device link")
}

impl IsolationBackend for ReflinkBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Reflink
    }

    fn clones_tree(&self) -> bool {
        true
    }

    fn probe(&self, lower: &Path, ctx: Option<&IsolationContext>) -> Result<ProbeResult> {
        self.probe_inner(lower, ctx)
    }

    fn start(
        &self,
        lower: &Path,
        merged: &Path,
        ctx: &IsolationContext,
    ) -> Result<Option<StartDetail>> {
        if self.io.platform() != "linux" {
            return Err(IsolationError::unavailable("FICLONE requires Linux"));
        }
        if exists(merged)? {
            return Err(IsolationError::other(format!(
                "reflink destination already exists: {}",
                merged.display()
            )));
        }
        if let Some(parent) = merged.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if ctx.cross_device
            || self.io.device(lower)? != self.io.device(&dirname(merged))?
        {
            return Err(IsolationError::unavailable("reflink requires the same device"));
        }
        // Direct start callers get the same lazy initialization as probe callers.
        if !lock(&self.state).loaded {
            let probe = self.probe_inner(lower, Some(ctx))?;
            if !probe.available {
                return Err(IsolationError::unavailable(
                    probe.reason.unwrap_or_default(),
                ));
            }
        }
        let ffi = lock(&self.state).ffi.clone();
        if let Some(reason) = lock(&self.state).unavailable_reason.clone() {
            return Err(IsolationError::unavailable(reason));
        }
        let result = (|| -> Result<()> {
            match &ffi {
                Some(ffi) => {
                    let mut budget = copy_budget(&ctx.base_dir, ctx.max_copy_bytes, None)?;
                    copy_tree(
                        lower,
                        merged,
                        &mut |source, destination, _size| {
                            self.clone_file(ffi, source, destination)
                        },
                        &mut |size| budget.consume(size),
                    )?;
                }
                None => self.cp(lower, merged)?,
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                mark_started(&ctx.base_dir, self.kind(), &[])?;
                Ok(None)
            }
            Err(error) => {
                let _ = std::fs::remove_dir_all(merged);
                Err(error)
            }
        }
    }

    fn stop(&self, merged: &Path) -> Result<()> {
        match std::fs::remove_dir_all(merged) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}
