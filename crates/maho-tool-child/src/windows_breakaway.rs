//! Starting a command outside the tool child's Job Object (senpi#3566, plan todo 4): the only way an explicit
//! `detached: true` launch on Windows can outlive the child. `CreateProcessW` with `CREATE_BREAKAWAY_FROM_JOB`
//! (allowed because the job sets `JOB_OBJECT_LIMIT_BREAKAWAY_OK`); the command inherits the caller's standard handles
//! and an explicit environment block with every internal variable removed. Struct layouts are the x64 ones
//! (`STARTUPINFOW` 104 bytes, `PROCESS_INFORMATION` 24 bytes).
//!
//! senpi `modes/rpc/tool-child/windows-breakaway.ts`. Upstream reaches kernel32 through `bun:ffi`; here the real
//! `Kernel32Process` calls it through `windows-sys` (Windows builds only) and other platforms have none.
// SAFETY (module-wide): the only `unsafe` is in `real`, one documented Win32 call per block; see there.
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::sync::Arc;

use crate::launch_env_names::without_internal_env_on;

const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
const CREATE_UNICODE_ENVIRONMENT: u32 = 0x0000_0400;
const STARTF_USESTDHANDLES: u32 = 0x0000_0100;
const STARTUPINFOW_BYTES: usize = 104;
const STARTUPINFO_FLAGS_OFFSET: usize = 60;
const STARTUPINFO_STD_INPUT_OFFSET: usize = 80;
const PROCESS_INFORMATION_BYTES: usize = 24;
const PROCESS_ID_OFFSET: usize = 16;
const INFINITE: u32 = 0xffff_ffff;
const STD_HANDLES: [i32; 3] = [-10, -11, -12];

/// The kernel32 surface the breakaway launch needs. Handles are `u64`; buffers are the raw x64 struct bytes.
pub trait Kernel32Process: Send + Sync {
    fn create_process_w(
        &self,
        command_line: &[u8],
        flags: u32,
        environment: &[u8],
        startup_info: &[u8],
        process_info: &mut [u8],
    ) -> i32;
    fn get_std_handle(&self, which: i32) -> u64;
    fn wait_for_single_object(&self, handle: u64, ms: u32) -> u32;
    fn get_exit_code_process(&self, handle: u64, exit_code: &mut [u8]) -> i32;
    fn close_handle(&self, handle: u64) -> i32;
    fn get_last_error(&self) -> u32;
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("tool_child_breakaway_failed: {step} failed (win32 error {win32_error})")]
pub struct BreakawayError {
    pub step: String,
    pub win32_error: u32,
}

impl BreakawayError {
    pub const CODE: &'static str = "tool_child_breakaway_failed";

    pub fn new(step: &str, win32_error: u32) -> Self {
        Self {
            step: step.to_string(),
            win32_error,
        }
    }

    pub fn code(&self) -> &'static str {
        Self::CODE
    }
}

fn utf16z(text: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity((text.len() + 1) * 2);
    for unit in text.encode_utf16().chain(std::iter::once(0)) {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

/// A `CREATE_UNICODE_ENVIRONMENT` block: `NAME=value\0` entries, sorted by name as Windows expects, then one more `\0`.
pub fn environment_block(env: &HashMap<String, String>) -> Vec<u8> {
    // The filter runs with the win32 platform: Windows environment names are case-insensitive.
    let mut entries: Vec<(String, String)> =
        without_internal_env_on(env, true).into_iter().collect();
    entries.sort_by_key(|(name, _)| name.to_uppercase());
    // An empty block is still two NULs: one for the (absent) last entry, one ending the block.
    if entries.is_empty() {
        return utf16z("\0");
    }
    let mut text = String::new();
    for (name, value) in &entries {
        text.push_str(name);
        text.push('=');
        text.push_str(value);
        text.push('\0');
    }
    utf16z(&text)
}

pub struct BreakawayProcess {
    pub pid: u32,
    kernel: Arc<dyn Kernel32Process>,
    process: u64,
}

impl BreakawayProcess {
    /// Blocks until the command exits and returns its exit code.
    pub fn wait(&self) -> Result<u32, BreakawayError> {
        self.kernel.wait_for_single_object(self.process, INFINITE);
        let mut code = [0u8; 4];
        let ok = self.kernel.get_exit_code_process(self.process, &mut code) != 0;
        self.kernel.close_handle(self.process);
        if !ok {
            return Err(BreakawayError::new(
                "GetExitCodeProcess",
                self.kernel.get_last_error(),
            ));
        }
        Ok(u32::from_le_bytes(code))
    }
}

pub fn start_breakaway(
    kernel: Arc<dyn Kernel32Process>,
    command_line: &str,
    env: &HashMap<String, String>,
) -> Result<BreakawayProcess, BreakawayError> {
    let mut startup_info = vec![0u8; STARTUPINFOW_BYTES];
    startup_info[0..4].copy_from_slice(&(STARTUPINFOW_BYTES as u32).to_le_bytes());
    startup_info[STARTUPINFO_FLAGS_OFFSET..STARTUPINFO_FLAGS_OFFSET + 4]
        .copy_from_slice(&STARTF_USESTDHANDLES.to_le_bytes());
    for (index, which) in STD_HANDLES.iter().enumerate() {
        let at = STARTUPINFO_STD_INPUT_OFFSET + index * 8;
        startup_info[at..at + 8].copy_from_slice(&kernel.get_std_handle(*which).to_le_bytes());
    }
    let mut process_info = vec![0u8; PROCESS_INFORMATION_BYTES];
    let flags = CREATE_BREAKAWAY_FROM_JOB | CREATE_UNICODE_ENVIRONMENT;
    if kernel.create_process_w(
        &utf16z(command_line),
        flags,
        &environment_block(env),
        &startup_info,
        &mut process_info,
    ) == 0
    {
        return Err(BreakawayError::new(
            "CreateProcessW",
            kernel.get_last_error(),
        ));
    }
    let read_u64 = |at: usize| {
        let mut raw = [0u8; 8];
        raw.copy_from_slice(&process_info[at..at + 8]);
        u64::from_le_bytes(raw)
    };
    let process = read_u64(0);
    kernel.close_handle(read_u64(8));
    let mut pid = [0u8; 4];
    pid.copy_from_slice(&process_info[PROCESS_ID_OFFSET..PROCESS_ID_OFFSET + 4]);
    Ok(BreakawayProcess {
        pid: u32::from_le_bytes(pid),
        kernel,
        process,
    })
}

/// The real kernel32, or `None` off Windows (upstream: `process.platform !== "win32"` or no `bun:ffi`).
pub fn load_kernel32_process() -> Option<Arc<dyn Kernel32Process>> {
    #[cfg(windows)]
    {
        Some(Arc::new(real::RealKernel32Process))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(windows)]
mod real {
    use super::Kernel32Process;
    use std::ffi::c_void;
    use std::ptr;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE};
    use windows_sys::Win32::System::Console::GetStdHandle;
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, GetExitCodeProcess, PROCESS_INFORMATION, STARTUPINFOW, WaitForSingleObject,
    };

    pub struct RealKernel32Process;

    fn to_u16(bytes: &[u8]) -> Vec<u16> {
        bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect()
    }

    impl Kernel32Process for RealKernel32Process {
        fn create_process_w(
            &self,
            command_line: &[u8],
            flags: u32,
            environment: &[u8],
            startup_info: &[u8],
            process_info: &mut [u8],
        ) -> i32 {
            // CreateProcessW may modify the command line, so it gets its own writable NUL-terminated copy.
            let mut line = to_u16(command_line);
            let env = to_u16(environment);
            // The x64 byte layouts upstream builds are exactly these structs (104 and 24 bytes).
            // SAFETY: both are plain-old-data structs for which all-zero is valid.
            let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
            let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
            let copy = startup_info.len().min(std::mem::size_of::<STARTUPINFOW>());
            // SAFETY: `copy` bytes fit in both the source slice and the destination struct.
            unsafe {
                ptr::copy_nonoverlapping(
                    startup_info.as_ptr(),
                    ptr::addr_of_mut!(startup).cast::<u8>(),
                    copy,
                );
            }
            // SAFETY: `line` is a writable NUL-terminated UTF-16 buffer, `env` a double-NUL-terminated Unicode block
            // (CREATE_UNICODE_ENVIRONMENT is in `flags`), and `startup`/`info` are live structs; the std handles
            // inside `startup` are the caller's own.
            let ok = unsafe {
                CreateProcessW(
                    ptr::null(),
                    line.as_mut_ptr(),
                    ptr::null(),
                    ptr::null(),
                    1,
                    flags,
                    env.as_ptr().cast::<c_void>(),
                    ptr::null(),
                    &startup,
                    &mut info,
                )
            };
            if ok != 0 && process_info.len() >= 24 {
                process_info[0..8].copy_from_slice(&(info.hProcess as usize as u64).to_le_bytes());
                process_info[8..16].copy_from_slice(&(info.hThread as usize as u64).to_le_bytes());
                process_info[16..20].copy_from_slice(&info.dwProcessId.to_le_bytes());
                process_info[20..24].copy_from_slice(&info.dwThreadId.to_le_bytes());
            }
            ok
        }

        fn get_std_handle(&self, which: i32) -> u64 {
            // SAFETY: `which` is one of the documented STD_*_HANDLE constants (-10, -11, -12).
            unsafe { GetStdHandle(which as u32) as usize as u64 }
        }

        fn wait_for_single_object(&self, handle: u64, ms: u32) -> u32 {
            // SAFETY: `handle` is the process handle CreateProcessW returned and the caller still owns.
            unsafe { WaitForSingleObject(handle as usize as HANDLE, ms) }
        }

        fn get_exit_code_process(&self, handle: u64, exit_code: &mut [u8]) -> i32 {
            let mut code: u32 = 0;
            // SAFETY: `handle` is an open process handle; `code` is a live DWORD output.
            let ok = unsafe { GetExitCodeProcess(handle as usize as HANDLE, &mut code) };
            if exit_code.len() >= 4 {
                exit_code[..4].copy_from_slice(&code.to_le_bytes());
            }
            ok
        }

        fn close_handle(&self, handle: u64) -> i32 {
            // SAFETY: `handle` is owned by the caller and closed exactly once.
            unsafe { CloseHandle(handle as usize as HANDLE) }
        }

        fn get_last_error(&self) -> u32 {
            // SAFETY: reads the calling thread's last-error value.
            unsafe { GetLastError() }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn decode_block(block: &[u8]) -> Vec<String> {
        let units: Vec<u16> = block
            .chunks_exact(2)
            .map(|p| u16::from_le_bytes([p[0], p[1]]))
            .collect();
        String::from_utf16_lossy(&units)
            .split('\0')
            .filter(|entry| !entry.is_empty())
            .map(str::to_string)
            .collect()
    }

    #[derive(Default)]
    struct Seen {
        command_line: String,
        startup: Vec<u8>,
        flags: u32,
        environment: Vec<String>,
    }

    struct Fake {
        created: bool,
        calls: Mutex<Vec<String>>,
        seen: Mutex<Seen>,
    }

    impl Kernel32Process for Fake {
        fn create_process_w(
            &self,
            line: &[u8],
            flags: u32,
            block: &[u8],
            startup: &[u8],
            info: &mut [u8],
        ) -> i32 {
            let units: Vec<u16> = line
                .chunks_exact(2)
                .map(|p| u16::from_le_bytes([p[0], p[1]]))
                .collect();
            let mut seen = self.seen.lock().unwrap();
            seen.command_line = String::from_utf16_lossy(&units)
                .trim_end_matches('\0')
                .to_string();
            seen.environment = decode_block(block);
            seen.startup = startup.to_vec();
            seen.flags = flags;
            if !self.created {
                return 0;
            }
            info[0..8].copy_from_slice(&0x111u64.to_le_bytes());
            info[8..16].copy_from_slice(&0x222u64.to_le_bytes());
            info[16..20].copy_from_slice(&5150u32.to_le_bytes());
            1
        }
        fn get_std_handle(&self, which: i32) -> u64 {
            (1000 - i64::from(which)) as u64
        }
        fn wait_for_single_object(&self, handle: u64, _ms: u32) -> u32 {
            self.calls.lock().unwrap().push(format!("wait:{handle}"));
            0
        }
        fn get_exit_code_process(&self, handle: u64, code: &mut [u8]) -> i32 {
            self.calls.lock().unwrap().push(format!("exit:{handle}"));
            code[0..4].copy_from_slice(&3u32.to_le_bytes());
            1
        }
        fn close_handle(&self, handle: u64) -> i32 {
            self.calls.lock().unwrap().push(format!("close:{handle}"));
            1
        }
        fn get_last_error(&self) -> u32 {
            87
        }
    }

    fn fake(created: bool) -> Arc<Fake> {
        Arc::new(Fake {
            created,
            calls: Mutex::new(Vec::new()),
            seen: Mutex::new(Seen::default()),
        })
    }

    fn calls(kernel: &Fake) -> Vec<String> {
        kernel.calls.lock().unwrap().clone()
    }

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
    }

    fn u64_at(bytes: &[u8], at: usize) -> u64 {
        let mut raw = [0u8; 8];
        raw.copy_from_slice(&bytes[at..at + 8]);
        u64::from_le_bytes(raw)
    }

    #[test]
    fn passes_the_breakaway_flag_the_quoted_command_line_and_the_x64_startupinfow_with_the_callers_std_handles()
     {
        let kernel = fake(true);
        start_breakaway(kernel.clone(), "cmd /c \"echo hi\"", &HashMap::new()).unwrap();
        let seen = kernel.seen.lock().unwrap();
        assert_eq!(seen.flags & 0x0100_0000, 0x0100_0000);
        assert_eq!(seen.command_line, "cmd /c \"echo hi\"");
        assert_eq!(u32_at(&seen.startup, 0), 104);
        assert_eq!(u32_at(&seen.startup, 60), 0x100);
        let handles: Vec<u64> = [80, 88, 96]
            .iter()
            .map(|at| u64_at(&seen.startup, *at))
            .collect();
        assert_eq!(handles, vec![1010, 1011, 1012]);
    }

    #[test]
    fn reads_the_pid_from_process_information_closes_the_thread_handle_at_once_and_the_process_handle_after_the_wait()
     {
        let kernel = fake(true);
        let command = start_breakaway(kernel.clone(), "x.exe", &HashMap::new()).unwrap();
        assert_eq!(command.pid, 5150);
        assert_eq!(calls(&kernel), vec!["close:546"]);
        assert_eq!(command.wait().unwrap(), 3);
        assert_eq!(
            calls(&kernel),
            vec!["close:546", "wait:273", "exit:273", "close:273"]
        );
    }

    #[test]
    fn fails_typed_when_create_process_w_refuses() {
        let kernel = fake(false);
        let error = start_breakaway(kernel, "x.exe", &HashMap::new())
            .err()
            .unwrap();
        assert_eq!(error.code(), "tool_child_breakaway_failed");
        assert_eq!(
            error.to_string(),
            "tool_child_breakaway_failed: CreateProcessW failed (win32 error 87)"
        );
    }

    #[test]
    fn hands_the_command_an_explicit_environment_with_every_internal_variable_removed() {
        // senpi#3590 review HIGH 1: with lpEnvironment = null the command inherited the shim's role and command line.
        let kernel = fake(true);
        let env: HashMap<String, String> = [
            ("Path", "C:\\bin"),
            ("SENPI_INTERNAL_ROLE", "launch-shim"),
            ("SENPI_LAUNCH_SHIM_COMMAND_LINE", "x.exe"),
            ("SENPI_TOOL_CHILD_REENTRY", "{}"),
            ("USERNAME", "u"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        start_breakaway(kernel.clone(), "x.exe", &env).unwrap();
        let seen = kernel.seen.lock().unwrap();
        assert_eq!(seen.flags & 0x400, 0x400);
        assert_eq!(seen.environment, vec!["Path=C:\\bin", "USERNAME=u"]);
    }

    #[test]
    fn an_empty_environment_is_still_two_nuls() {
        assert_eq!(environment_block(&HashMap::new()), vec![0, 0, 0, 0]);
    }

    #[cfg(not(windows))]
    #[test]
    fn has_no_kernel_off_windows() {
        assert!(load_kernel32_process().is_none());
    }
}
