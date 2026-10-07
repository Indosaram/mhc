use std::ffi::{CStr, CString};
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::backend::{
    BackendKind, IsolationBackend, IsolationContext, IsolationError, ProbeResult, Result,
    StartDetail,
};
use crate::backend_marker::mark_started;
use crate::util::{dirname, lock};

pub const CLONE_NOFOLLOW: u32 = 0x0001;

pub trait CloneSymbols: Send + Sync {
    fn clonefile(&self, src: &CStr, dst: &CStr, flags: u32) -> i32;
    fn errno(&self) -> i32;
    fn strerror(&self, errno: i32) -> String;
}

pub use crate::backends::NativeLoadError as ApfsLoadError;

pub type LoadApfs =
    Arc<dyn Fn() -> std::result::Result<Arc<dyn CloneSymbols>, ApfsLoadError> + Send + Sync>;

fn cstring(path: &Path) -> Result<CString> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        CString::new(path.as_os_str().as_bytes())
            .map_err(|error| IsolationError::other(error.to_string()))
    }
    #[cfg(not(unix))]
    {
        CString::new(path.to_string_lossy().into_owned())
            .map_err(|error| IsolationError::other(error.to_string()))
    }
}

pub fn clone_with_symbols(
    symbols: &Arc<dyn CloneSymbols>,
    source: &Path,
    destination: &Path,
) -> Result<()> {
    let src = cstring(source)?;
    let dst = cstring(destination)?;
    if symbols.clonefile(&src, &dst, CLONE_NOFOLLOW) == 0 {
        return Ok(());
    }
    // Nothing may call into foreign code between clonefile and reading thread-local errno.
    let errno = symbols.errno();
    let message = format!(
        "clonefile {} -> {}: {}",
        source.display(),
        destination.display(),
        symbols.strerror(errno)
    );
    if [45, 102, 18, 1].contains(&errno) {
        return Err(IsolationError::unavailable(message));
    }
    Err(IsolationError::Clone { errno, message })
}

#[cfg(target_os = "macos")]
mod system {
    use super::{ApfsLoadError, CloneSymbols};
    use std::ffi::CStr;
    use std::sync::Arc;

    extern "C" {
        fn clonefile(src: *const libc::c_char, dst: *const libc::c_char, flags: u32) -> libc::c_int;
    }

    pub struct SystemCloneSymbols;

    impl CloneSymbols for SystemCloneSymbols {
        fn clonefile(&self, src: &CStr, dst: &CStr, flags: u32) -> i32 {
            unsafe { clonefile(src.as_ptr(), dst.as_ptr(), flags) }
        }

        fn errno(&self) -> i32 {
            std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
        }

        fn strerror(&self, errno: i32) -> String {
            unsafe {
                let pointer = libc::strerror(errno);
                if pointer.is_null() {
                    return String::new();
                }
                CStr::from_ptr(pointer).to_string_lossy().into_owned()
            }
        }
    }

    pub fn load() -> Result<Arc<dyn CloneSymbols>, ApfsLoadError> {
        Ok(Arc::new(SystemCloneSymbols))
    }
}

#[cfg(target_os = "macos")]
pub fn default_load_apfs() -> std::result::Result<Arc<dyn CloneSymbols>, ApfsLoadError> {
    system::load()
}

#[cfg(not(target_os = "macos"))]
pub fn default_load_apfs() -> std::result::Result<Arc<dyn CloneSymbols>, ApfsLoadError> {
    Err(ApfsLoadError {
        code: Some("ERR_UNKNOWN_BUILTIN_MODULE".to_string()),
        message: "APFS clonefile requires macOS".to_string(),
    })
}

pub struct ApfsBackend {
    load: LoadApfs,
    platform: String,
    symbols: Mutex<Option<Arc<dyn CloneSymbols>>>,
}

impl Default for ApfsBackend {
    fn default() -> Self {
        ApfsBackend {
            load: Arc::new(default_load_apfs),
            platform: crate::backend::platform_name().to_string(),
            symbols: Mutex::new(None),
        }
    }
}

impl ApfsBackend {
    pub fn new(load: LoadApfs) -> Self {
        ApfsBackend {
            load,
            platform: crate::backend::platform_name().to_string(),
            symbols: Mutex::new(None),
        }
    }

    pub fn with_platform(load: LoadApfs, platform: &str) -> Self {
        ApfsBackend {
            load,
            platform: platform.to_string(),
            symbols: Mutex::new(None),
        }
    }

    fn probe_inner(&self) -> Result<ProbeResult> {
        if self.platform != "darwin" {
            return Ok(ProbeResult::unavailable("APFS requires macOS"));
        }
        if lock(&self.symbols).is_none() {
            match (self.load)() {
                Ok(symbols) => *lock(&self.symbols) = Some(symbols),
                Err(cause) => {
                    // A missing native module is an unavailable capability; anything else
                    // (a failed dlopen, for example) is an operational failure that propagates.
                    if let Some(code) = &cause.code
                        && [
                            "ERR_UNSUPPORTED_ESM_URL_SCHEME",
                            "ERR_UNKNOWN_BUILTIN_MODULE",
                            "MODULE_NOT_FOUND",
                            "ERR_MODULE_NOT_FOUND",
                        ]
                        .contains(&code.as_str())
                    {
                        return Err(IsolationError::unavailable(format!(
                            "APFS clonefile unavailable: {}",
                            cause.message
                        )));
                    }
                    return Err(IsolationError::other(cause.message));
                }
            }
        }
        Ok(ProbeResult::available())
    }
}

impl IsolationBackend for ApfsBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Apfs
    }

    fn clones_tree(&self) -> bool {
        true
    }

    fn probe(&self, _repo_root: &Path, _ctx: Option<&IsolationContext>) -> Result<ProbeResult> {
        self.probe_inner()
    }

    fn start(
        &self,
        lower: &Path,
        merged: &Path,
        ctx: &IsolationContext,
    ) -> Result<Option<StartDetail>> {
        if ctx.cross_device {
            return Err(IsolationError::unavailable("APFS requires the same device"));
        }
        let probe = self.probe_inner()?;
        if !probe.available {
            return Err(IsolationError::unavailable(
                probe.reason.unwrap_or_default(),
            ));
        }
        std::fs::create_dir_all(dirname(merged))?;
        if crate::util::device_of(&std::fs::metadata(lower)?)
            != crate::util::device_of(&std::fs::metadata(dirname(merged))?)
        {
            return Err(IsolationError::unavailable("APFS requires the same device"));
        }
        let symbols = lock(&self.symbols).clone();
        let Some(symbols) = symbols else {
            return Err(IsolationError::unavailable("APFS clonefile unavailable"));
        };
        // Some Darwin versions clone special entries instead of rejecting them. Inspect the
        // completed snapshot, not the source, so the fast path still starts with one syscall.
        let fast = clone_with_symbols(&symbols, lower, merged);
        let fast_ok = match fast {
            Ok(()) => !has_special_entries(merged),
            Err(error) => match &error {
                IsolationError::Clone { errno, .. } if [22, 45, 102].contains(errno) => false,
                _ => return Err(error),
            },
        };
        if fast_ok {
            mark_started(&ctx.base_dir, self.kind(), &[])?;
            return Ok(Some(StartDetail {
                strategy_detail: "clonefile".to_string(),
            }));
        }
        let _ = std::fs::remove_dir_all(merged);
        fn walk(symbols: &Arc<dyn CloneSymbols>, src: &Path, dst: &Path) -> Result<()> {
            let info = std::fs::symlink_metadata(src)?;
            if !info.is_dir() {
                if info.is_file() || info.file_type().is_symlink() {
                    clone_with_symbols(symbols, src, dst)?;
                }
                return Ok(());
            }
            std::fs::create_dir(dst)?;
            for entry in std::fs::read_dir(src)? {
                let entry = entry?;
                walk(symbols, &entry.path(), &dst.join(entry.file_name()))?;
            }
            Ok(())
        }
        walk(&symbols, lower, merged)?;
        mark_started(&ctx.base_dir, self.kind(), &[])?;
        Ok(Some(StartDetail {
            strategy_detail: "clone_tree".to_string(),
        }))
    }

    fn stop(&self, merged: &Path) -> Result<()> {
        match std::fs::remove_dir_all(merged) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

fn has_special_entries(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if has_special_entries(&entry.path()) {
                return true;
            }
        } else if !file_type.is_file() && !file_type.is_symlink() {
            return true;
        }
    }
    false
}
