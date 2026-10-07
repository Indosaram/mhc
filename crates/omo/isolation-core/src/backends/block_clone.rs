use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::backend::{
    BackendKind, IsolationBackend, IsolationContext, IsolationError, ProbeResult, Result,
    StartDetail,
};
use crate::backend_marker::mark_started;
use crate::backends::btrfs::remove_dir_all_force;
use crate::backends::copy_tree::copy_tree;
use crate::backends::runtime::{existing_parent, BackendRuntime, SystemRuntime};
use crate::backends::NativeLoadError;
use crate::git::command::exists;
use crate::util::{dirname, lock};

const FSCTL_DUPLICATE_EXTENTS_TO_FILE: u32 = 0x00098344;

pub trait WindowsSymbols: Send + Sync {
    fn create_file_w(
        &self,
        path: &[u8],
        access: u32,
        share: u32,
        disposition: u32,
        flags: u32,
    ) -> i64;
    fn set_file_pointer_ex(&self, handle: i64, offset: u64) -> i32;
    fn set_end_of_file(&self, handle: i64) -> i32;
    fn device_io_control(&self, handle: i64, code: u32, input: &[u8], returned: &mut [u8]) -> i32;
    fn close_handle(&self, handle: i64) -> i32;
    fn get_last_error(&self) -> u32;
    fn get_disk_free_space_w(&self, root: &[u8], sectors: &mut [u8], bytes: &mut [u8]) -> i32;
}

pub trait WindowsCloneApi: Send + Sync {
    fn open(&self, path: &str, write: bool) -> Result<u64>;
    fn resize(&self, handle: u64, size: u64) -> Result<()>;
    fn duplicate(&self, destination: u64, source: u64, bytes: u64) -> Result<()>;
    fn close(&self, handle: u64) -> Result<()>;
    fn cluster_size(&self, path: &str) -> Result<u64>;
}

pub type LoadWindowsApi =
    Arc<dyn Fn() -> std::result::Result<Arc<dyn WindowsCloneApi>, NativeLoadError> + Send + Sync>;

fn wide(path: &str) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(path.len() * 2 + 2);
    for unit in path.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out.extend_from_slice(&[0, 0]);
    out
}

fn to_namespaced_path(path: &str) -> String {
    if path.starts_with(r"\\?\") {
        return path.to_string();
    }
    if let Some(rest) = path.strip_prefix(r"\\") {
        return format!(r"\\?\UNC\{rest}");
    }
    format!(r"\\?\{path}")
}

fn windows_root(path: &str) -> String {
    let trimmed = path.strip_prefix(r"\\?\").unwrap_or(path);
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() >= 2 && chars[1] == ':' {
        return format!("{}:\\", chars[0]);
    }
    let parts: Vec<&str> = trimmed.trim_start_matches('\\').splitn(3, '\\').collect();
    if parts.len() >= 2 && !parts[0].is_empty() {
        return format!("\\\\{}\\{}", parts[0], parts[1]);
    }
    trimmed.to_string()
}

struct SymbolCloneApi {
    symbols: Arc<dyn WindowsSymbols>,
}

impl SymbolCloneApi {
    fn fail(&self, operation: &str) -> IsolationError {
        let errno = self.symbols.get_last_error();
        let message = format!("{operation}: Windows error {errno}");
        if [1, 17, 50, 87].contains(&errno) {
            return IsolationError::unavailable(message);
        }
        IsolationError::other(message)
    }
}

impl WindowsCloneApi for SymbolCloneApi {
    fn open(&self, path: &str, write: bool) -> Result<u64> {
        let handle = self.symbols.create_file_w(
            &wide(path),
            if write { 0x40000000 } else { 0x80000000 },
            7,
            if write { 1 } else { 3 },
            0x80,
        );
        if handle == 0 || handle == -1 || handle as u64 >= 9_007_199_254_740_991 {
            return Err(self.fail("CreateFileW"));
        }
        Ok(handle as u64)
    }

    fn resize(&self, handle: u64, size: u64) -> Result<()> {
        if self.symbols.set_file_pointer_ex(handle as i64, size) == 0 {
            return Err(self.fail("SetFilePointerEx"));
        }
        if self.symbols.set_end_of_file(handle as i64) == 0 {
            return Err(self.fail("SetEndOfFile"));
        }
        Ok(())
    }

    fn duplicate(&self, destination: u64, source: u64, bytes: u64) -> Result<()> {
        let mut data = vec![0u8; 32];
        data[0..8].copy_from_slice(&source.to_le_bytes());
        data[24..32].copy_from_slice(&bytes.to_le_bytes());
        let mut returned = vec![0u8; 4];
        if self.symbols.device_io_control(
            destination as i64,
            FSCTL_DUPLICATE_EXTENTS_TO_FILE,
            &data,
            &mut returned,
        ) == 0
        {
            return Err(self.fail("DeviceIoControl"));
        }
        Ok(())
    }

    fn close(&self, handle: u64) -> Result<()> {
        if self.symbols.close_handle(handle as i64) == 0 {
            return Err(self.fail("CloseHandle"));
        }
        Ok(())
    }

    fn cluster_size(&self, path: &str) -> Result<u64> {
        let mut sectors = vec![0u8; 4];
        let mut bytes = vec![0u8; 4];
        if self
            .symbols
            .get_disk_free_space_w(&wide(&windows_root(path)), &mut sectors, &mut bytes)
            == 0
        {
            return Err(self.fail("GetDiskFreeSpaceW"));
        }
        let sectors = u32::from_le_bytes([sectors[0], sectors[1], sectors[2], sectors[3]]);
        let bytes = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        Ok(sectors as u64 * bytes as u64)
    }
}

pub fn create_windows_clone_api(symbols: Arc<dyn WindowsSymbols>) -> Arc<dyn WindowsCloneApi> {
    Arc::new(SymbolCloneApi { symbols })
}

pub fn duplicate_extents(
    api: &dyn WindowsCloneApi,
    source: &str,
    destination: &str,
    size: u64,
) -> Result<()> {
    let src = to_namespaced_path(source);
    let dst = to_namespaced_path(destination);
    let cluster = api.cluster_size(&src)?;
    if size < cluster {
        std::fs::copy(source, destination)?;
        return Ok(());
    }
    let aligned = (size / cluster) * cluster;
    let input = api.open(&src, false)?;
    let result = (|| -> Result<()> {
        let output = api.open(&dst, true)?;
        let inner = (|| -> Result<()> {
            api.resize(output, size.div_ceil(cluster) * cluster)?;
            api.duplicate(output, input, aligned)?;
            api.resize(output, size)?;
            Ok(())
        })();
        let _ = api.close(output);
        inner
    })();
    let _ = api.close(input);
    result?;
    if aligned < size {
        let mut input_file = std::fs::File::open(source)?;
        let mut output_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(destination)?;
        let tail_length = (size - aligned) as usize;
        let mut tail = vec![0u8; tail_length];
        input_file.seek(SeekFrom::Start(aligned))?;
        let read = input_file.read(&mut tail)?;
        if read != tail_length {
            return Err(IsolationError::other("Source changed during ReFS tail copy"));
        }
        output_file.seek(SeekFrom::Start(aligned))?;
        let mut written = 0usize;
        while written < tail.len() {
            let wrote = output_file.write(&tail[written..])?;
            if wrote == 0 {
                return Err(IsolationError::other("ReFS tail copy made no progress"));
            }
            written += wrote;
        }
    }
    Ok(())
}

pub fn default_load_windows() -> std::result::Result<Arc<dyn WindowsCloneApi>, NativeLoadError> {
    #[cfg(windows)]
    {
        return Ok(Arc::new(create_windows_clone_api(Arc::new(
            crate::backends::block_clone::kernel32::Kernel32,
        ))));
    }
    #[cfg(not(windows))]
    {
        Err(NativeLoadError {
            code: Some("ERR_UNKNOWN_BUILTIN_MODULE".to_string()),
            message: "Windows block clone requires Windows".to_string(),
        })
    }
}

#[cfg(windows)]
mod kernel32 {
    use super::WindowsSymbols;

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateFileW(
            path: *const u16,
            access: u32,
            share: u32,
            security: *mut std::ffi::c_void,
            disposition: u32,
            flags: u32,
            template: *mut std::ffi::c_void,
        ) -> *mut std::ffi::c_void;
        fn SetFilePointerEx(
            handle: *mut std::ffi::c_void,
            offset: i64,
            output: *mut i64,
            method: u32,
        ) -> i32;
        fn SetEndOfFile(handle: *mut std::ffi::c_void) -> i32;
        fn DeviceIoControl(
            handle: *mut std::ffi::c_void,
            code: u32,
            input: *const u8,
            input_length: u32,
            output: *mut u8,
            output_length: u32,
            returned: *mut u32,
            overlapped: *mut std::ffi::c_void,
        ) -> i32;
        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
        fn GetLastError() -> u32;
        fn GetDiskFreeSpaceW(
            root: *const u16,
            sectors: *mut u32,
            bytes: *mut u32,
            free: *mut u32,
            total: *mut u32,
        ) -> i32;
    }

    pub struct Kernel32;

    impl WindowsSymbols for Kernel32 {
        fn create_file_w(
            &self,
            path: &[u8],
            access: u32,
            share: u32,
            disposition: u32,
            flags: u32,
        ) -> i64 {
            let handle = unsafe {
                CreateFileW(
                    path.as_ptr() as *const u16,
                    access,
                    share,
                    std::ptr::null_mut(),
                    disposition,
                    flags,
                    std::ptr::null_mut(),
                )
            };
            handle as i64
        }

        fn set_file_pointer_ex(&self, handle: i64, offset: u64) -> i32 {
            unsafe {
                SetFilePointerEx(
                    handle as *mut std::ffi::c_void,
                    offset as i64,
                    std::ptr::null_mut(),
                    0,
                )
            }
        }

        fn set_end_of_file(&self, handle: i64) -> i32 {
            unsafe { SetEndOfFile(handle as *mut std::ffi::c_void) }
        }

        fn device_io_control(
            &self,
            handle: i64,
            code: u32,
            input: &[u8],
            returned: &mut [u8],
        ) -> i32 {
            unsafe {
                DeviceIoControl(
                    handle as *mut std::ffi::c_void,
                    code,
                    input.as_ptr(),
                    input.len() as u32,
                    std::ptr::null_mut(),
                    0,
                    returned.as_mut_ptr() as *mut u32,
                    std::ptr::null_mut(),
                )
            }
        }

        fn close_handle(&self, handle: i64) -> i32 {
            unsafe { CloseHandle(handle as *mut std::ffi::c_void) }
        }

        fn get_last_error(&self) -> u32 {
            unsafe { GetLastError() }
        }

        fn get_disk_free_space_w(&self, root: &[u8], sectors: &mut [u8], bytes: &mut [u8]) -> i32 {
            unsafe {
                GetDiskFreeSpaceW(
                    root.as_ptr() as *const u16,
                    sectors.as_mut_ptr() as *mut u32,
                    bytes.as_mut_ptr() as *mut u32,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            }
        }
    }
}

pub struct BlockCloneBackend {
    io: Arc<dyn BackendRuntime>,
    load: LoadWindowsApi,
    api: Mutex<Option<Arc<dyn WindowsCloneApi>>>,
}

impl Default for BlockCloneBackend {
    fn default() -> Self {
        BlockCloneBackend {
            io: Arc::new(SystemRuntime),
            load: Arc::new(default_load_windows),
            api: Mutex::new(None),
        }
    }
}

impl BlockCloneBackend {
    pub fn new(io: Arc<dyn BackendRuntime>, load: LoadWindowsApi) -> Self {
        BlockCloneBackend {
            io,
            load,
            api: Mutex::new(None),
        }
    }

    fn probe_inner(&self, lower: &Path, ctx: Option<&IsolationContext>) -> Result<ProbeResult> {
        if self.io.platform() != "win32" || !self.io.which("fsutil") {
            return Ok(ProbeResult::unavailable(
                "ReFS block clone requires Windows and fsutil",
            ));
        }
        if let Some(ctx) = ctx {
            let parent = existing_parent(&ctx.base_dir)?;
            if ctx.cross_device || self.io.device(lower)? != self.io.device(&parent)? {
                return Ok(ProbeResult::unavailable(
                    "ReFS requires the same volume serial",
                ));
            }
        }
        let root = windows_root(&lower.to_string_lossy());
        let result = self.io.run(&[
            "fsutil".to_string(),
            "fsinfo".to_string(),
            "volumeinfo".to_string(),
            root,
        ])?;
        // A probe-time CLI failure says the ReFS capability cannot be established on
        // this volume (missing privileges, transient fsutil errors); it is a capability
        // gap the candidate walk can fall through on, not an operational failure.
        // start()'s failures stay hard errors.
        if result.code != 0 {
            return Ok(ProbeResult::unavailable(format!(
                "fsutil volumeinfo failed ({}): {}",
                result.code, result.stderr
            )));
        }
        if !result.stdout.to_lowercase().contains("refs") {
            return Ok(ProbeResult::unavailable("volume is not ReFS"));
        }
        if lock(&self.api).is_none() {
            match (self.load)() {
                Ok(api) => *lock(&self.api) = Some(api),
                Err(cause) => {
                    if let Some(code) = &cause.code {
                        if [
                            "ERR_UNSUPPORTED_ESM_URL_SCHEME",
                            "ERR_UNKNOWN_BUILTIN_MODULE",
                            "MODULE_NOT_FOUND",
                            "ERR_MODULE_NOT_FOUND",
                        ]
                        .contains(&code.as_str())
                        {
                            return Ok(ProbeResult::unavailable(
                                "Windows block clone requires native FFI",
                            ));
                        }
                    }
                    return Err(IsolationError::other(cause.message));
                }
            }
        }
        Ok(ProbeResult::available())
    }
}

impl IsolationBackend for BlockCloneBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::BlockClone
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
        if exists(merged)? {
            return Err(IsolationError::other(format!(
                "block-clone destination already exists: {}",
                merged.display()
            )));
        }
        if let Some(parent) = merged.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if ctx.cross_device || self.io.device(lower)? != self.io.device(&dirname(merged))? {
            return Err(IsolationError::unavailable(
                "ReFS requires the same volume serial",
            ));
        }
        let probe = self.probe_inner(lower, None)?;
        if !probe.available {
            return Err(IsolationError::unavailable(
                probe.reason.unwrap_or_default(),
            ));
        }
        let Some(api) = lock(&self.api).clone() else {
            return Err(IsolationError::unavailable(
                "Windows block clone requires native FFI",
            ));
        };
        let lower_ns = PathBuf::from(to_namespaced_path(&lower.to_string_lossy()));
        let merged_ns = PathBuf::from(to_namespaced_path(&merged.to_string_lossy()));
        let result = copy_tree(
            &lower_ns,
            &merged_ns,
            &mut |source, destination, size| {
                duplicate_extents(
                    api.as_ref(),
                    &source.to_string_lossy(),
                    &destination.to_string_lossy(),
                    size,
                )
            },
            &mut |_size| Ok(()),
        );
        match result {
            Ok(()) => {
                mark_started(&ctx.base_dir, self.kind(), &[])?;
                Ok(None)
            }
            Err(error) => {
                let _ = remove_dir_all_force(&merged_ns);
                Err(error)
            }
        }
    }

    fn stop(&self, merged: &Path) -> Result<()> {
        remove_dir_all_force(&PathBuf::from(to_namespaced_path(&merged.to_string_lossy())))
    }
}
