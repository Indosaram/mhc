//! The Windows containment handle for one tool child generation (senpi#3566, plan todo 4): a Job Object the server
//! creates and keeps, so the child's process tree can be terminated even after the child itself is gone.
//!
//! The job is created with `KILL_ON_JOB_CLOSE | BREAKAWAY_OK`: closing the server's handle ends every member, and a
//! launch that explicitly asked to be detached can leave the job with `CREATE_BREAKAWAY_FROM_JOB`. Membership evidence
//! is creation-time (`command_started`); the process-id listing here only confirms, never refutes. Upstream reaches
//! kernel32 through `bun:ffi`; here `RealKernel32Job` calls it through `windows-sys` (Windows builds only), and every
//! other platform has no kernel (`load_kernel32_job` is `None`), exactly as upstream on non-win32.
//!
//! senpi `modes/rpc/tool-child/windows-job.ts`. Handles are `usize` (0 is NULL), the Rust stand-in for FFI pointers.
// SAFETY (module-wide): the only `unsafe` is in `real`, one documented Win32 call per block; see there.
#![allow(unsafe_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

pub const JOB_OBJECT_LIMIT_BREAKAWAY_OK: u32 = 0x0000_0800;
pub const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;
pub const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS: i32 = 9;
pub const JOB_OBJECT_BASIC_PROCESS_ID_LIST_CLASS: i32 = 3;
/// `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` on a 64-bit Windows: 64-byte basic limits, 48-byte IO counters, 4 sizes.
pub const EXTENDED_LIMIT_INFORMATION_BYTES: usize = 144;
/// `BasicLimitInformation.LimitFlags`: after two LARGE_INTEGER time limits.
pub const LIMIT_FLAGS_OFFSET: usize = 16;
const PROCESS_ID_LIST_CAPACITY: usize = 1024;
const PROCESS_ID_LIST_BYTES: usize = 8 + PROCESS_ID_LIST_CAPACITY * 8;
const PROCESS_TERMINATE: u32 = 0x0001;
const PROCESS_SET_QUOTA: u32 = 0x0100;
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;

/// The kernel32 surface the job needs. Handles are `usize`; 0 is NULL. Buffers are the raw x64 struct bytes.
pub trait Kernel32Job: Send + Sync {
    fn create_job_object_w(&self) -> usize;
    fn set_information_job_object(
        &self,
        job: usize,
        info_class: i32,
        info: &[u8],
        length: u32,
    ) -> i32;
    fn query_information_job_object(
        &self,
        job: usize,
        info_class: i32,
        info: &mut [u8],
        length: u32,
    ) -> i32;
    fn open_process(&self, access: u32, inherit: i32, pid: u32) -> usize;
    fn assign_process_to_job_object(&self, job: usize, process: usize) -> i32;
    /// `job == None` asks about any job (`IsProcessInJob(process, NULL, ...)`).
    fn is_process_in_job(&self, process: usize, job: Option<usize>, result: &mut [u8]) -> i32;
    fn terminate_job_object(&self, job: usize, exit_code: u32) -> i32;
    fn close_handle(&self, handle: usize) -> i32;
    fn get_last_error(&self) -> u32;
    fn get_current_process(&self) -> usize;
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("tool_child_job_assign_failed: {step} failed (win32 error {win32_error})")]
pub struct WindowsJobError {
    pub step: String,
    pub win32_error: u32,
}

impl WindowsJobError {
    pub const CODE: &'static str = "tool_child_job_assign_failed";

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

pub fn extended_limit_information() -> Vec<u8> {
    let mut info = vec![0u8; EXTENDED_LIMIT_INFORMATION_BYTES];
    let flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK;
    info[LIMIT_FLAGS_OFFSET..LIMIT_FLAGS_OFFSET + 4].copy_from_slice(&flags.to_le_bytes());
    info
}

pub struct WindowsJob {
    closed: AtomicBool,
    kernel: Arc<dyn Kernel32Job>,
    handle: usize,
}

impl WindowsJob {
    fn new(kernel: Arc<dyn Kernel32Job>, handle: usize) -> Self {
        Self {
            closed: AtomicBool::new(false),
            kernel,
            handle,
        }
    }

    pub fn create(kernel: Arc<dyn Kernel32Job>) -> Result<WindowsJob, WindowsJobError> {
        let handle = kernel.create_job_object_w();
        if handle == 0 {
            return Err(WindowsJobError::new(
                "CreateJobObjectW",
                kernel.get_last_error(),
            ));
        }
        let info = extended_limit_information();
        if kernel.set_information_job_object(
            handle,
            JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS,
            &info,
            info.len() as u32,
        ) == 0
        {
            let error = kernel.get_last_error();
            kernel.close_handle(handle);
            return Err(WindowsJobError::new("SetInformationJobObject", error));
        }
        Ok(WindowsJob::new(kernel, handle))
    }

    pub fn assign(&self, pid: u32) -> Result<(), WindowsJobError> {
        let process = self.kernel.open_process(
            PROCESS_SET_QUOTA | PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            pid,
        );
        if process == 0 {
            return Err(WindowsJobError::new(
                "OpenProcess",
                self.kernel.get_last_error(),
            ));
        }
        let result = self.assign_opened(process);
        self.kernel.close_handle(process);
        result
    }

    fn assign_opened(&self, process: usize) -> Result<(), WindowsJobError> {
        if self
            .kernel
            .assign_process_to_job_object(self.handle, process)
            == 0
        {
            return Err(WindowsJobError::new(
                "AssignProcessToJobObject",
                self.kernel.get_last_error(),
            ));
        }
        let mut in_job = [0u8; 4];
        if self
            .kernel
            .is_process_in_job(process, Some(self.handle), &mut in_job)
            == 0
            || i32::from_le_bytes(in_job) == 0
        {
            return Err(WindowsJobError::new(
                "IsProcessInJob",
                self.kernel.get_last_error(),
            ));
        }
        Ok(())
    }

    /// Confirmation only: a running pid listed here is a member; absence never proves a creation-time member left.
    pub fn member_pids(&self) -> Vec<u64> {
        let mut list = vec![0u8; PROCESS_ID_LIST_BYTES];
        let length = list.len() as u32;
        if self.kernel.query_information_job_object(
            self.handle,
            JOB_OBJECT_BASIC_PROCESS_ID_LIST_CLASS,
            &mut list,
            length,
        ) == 0
        {
            return Vec::new();
        }
        let listed = u32::from_le_bytes([list[4], list[5], list[6], list[7]]) as usize;
        let count = listed.min(PROCESS_ID_LIST_CAPACITY);
        (0..count)
            .map(|index| {
                let at = 8 + index * 8;
                let mut raw = [0u8; 8];
                raw.copy_from_slice(&list[at..at + 8]);
                u64::from_le_bytes(raw)
            })
            .collect()
    }

    pub fn terminate(&self, exit_code: u32) {
        if !self.closed.load(Ordering::SeqCst) {
            self.kernel.terminate_job_object(self.handle, exit_code);
        }
    }

    pub fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.kernel.close_handle(self.handle);
    }
}

/// The real kernel32, or `None` off Windows (upstream: `process.platform !== "win32"` or no `bun:ffi`).
pub fn load_kernel32_job() -> Option<Arc<dyn Kernel32Job>> {
    #[cfg(windows)]
    {
        Some(Arc::new(real::RealKernel32Job))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

static DEFAULT_IN_JOB: OnceLock<bool> = OnceLock::new();

/// Whether this process runs inside any Job Object (`IsProcessInJob(GetCurrentProcess(), NULL)`). Off Windows it is
/// false: there is no job to be in. The answer for this host's own kernel is computed once.
pub fn current_process_in_job(kernel: Option<&dyn Kernel32Job>) -> bool {
    if let Some(kernel) = kernel {
        return query_in_job(kernel);
    }
    *DEFAULT_IN_JOB.get_or_init(|| match load_kernel32_job() {
        Some(loaded) => query_in_job(loaded.as_ref()),
        None => false,
    })
}

fn query_in_job(kernel: &dyn Kernel32Job) -> bool {
    let mut result = [0u8; 4];
    kernel.is_process_in_job(kernel.get_current_process(), None, &mut result) != 0
        && i32::from_le_bytes(result) != 0
}

#[cfg(windows)]
mod real {
    use super::Kernel32Job;
    use std::ffi::c_void;
    use std::ptr;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, QueryInformationJobObject,
        SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcess};

    pub struct RealKernel32Job;

    fn handle(value: usize) -> HANDLE {
        value as HANDLE
    }

    /// 8-byte-aligned scratch copy of a byte buffer: the Win32 structs hold pointer-sized fields.
    fn aligned(bytes: &[u8]) -> Vec<u64> {
        let mut words = vec![0u64; bytes.len().div_ceil(8)];
        for (index, byte) in bytes.iter().enumerate() {
            let shift = (index % 8) * 8;
            words[index / 8] |= u64::from(*byte) << shift;
        }
        words
    }

    fn copy_back(words: &[u64], bytes: &mut [u8]) {
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = (words[index / 8] >> ((index % 8) * 8)) as u8;
        }
    }

    impl Kernel32Job for RealKernel32Job {
        fn create_job_object_w(&self) -> usize {
            // SAFETY: both arguments are documented as optional NULLs; the call has no other preconditions.
            unsafe { CreateJobObjectW(ptr::null(), ptr::null()) as usize }
        }

        fn set_information_job_object(
            &self,
            job: usize,
            info_class: i32,
            info: &[u8],
            length: u32,
        ) -> i32 {
            let words = aligned(info);
            // SAFETY: `words` is an aligned, live copy of `info` at least `length` bytes long (callers pass info.len()).
            unsafe {
                SetInformationJobObject(
                    handle(job),
                    info_class,
                    words.as_ptr().cast::<c_void>(),
                    length,
                )
            }
        }

        fn query_information_job_object(
            &self,
            job: usize,
            info_class: i32,
            info: &mut [u8],
            length: u32,
        ) -> i32 {
            let mut words = aligned(info);
            // SAFETY: `words` is an aligned writable buffer of at least `length` bytes; the return length is not requested.
            let ok = unsafe {
                QueryInformationJobObject(
                    handle(job),
                    info_class,
                    words.as_mut_ptr().cast::<c_void>(),
                    length,
                    ptr::null_mut(),
                )
            };
            copy_back(&words, info);
            ok
        }

        fn open_process(&self, access: u32, inherit: i32, pid: u32) -> usize {
            // SAFETY: plain value arguments; a NULL result is reported to the caller as failure.
            unsafe { OpenProcess(access, inherit, pid) as usize }
        }

        fn assign_process_to_job_object(&self, job: usize, process: usize) -> i32 {
            // SAFETY: the handles were obtained from this kernel and are still open (the caller owns them).
            unsafe { AssignProcessToJobObject(handle(job), handle(process)) }
        }

        fn is_process_in_job(&self, process: usize, job: Option<usize>, result: &mut [u8]) -> i32 {
            let mut out: i32 = 0;
            let job = job.map_or(ptr::null_mut(), handle);
            // SAFETY: `out` is a live BOOL-sized output; `job` is NULL (any job) or an open job handle.
            let ok = unsafe { IsProcessInJob(handle(process), job, &mut out) };
            if result.len() >= 4 {
                result[..4].copy_from_slice(&out.to_le_bytes());
            }
            ok
        }

        fn terminate_job_object(&self, job: usize, exit_code: u32) -> i32 {
            // SAFETY: `job` is an open job handle owned by the caller.
            unsafe { TerminateJobObject(handle(job), exit_code) }
        }

        fn close_handle(&self, value: usize) -> i32 {
            // SAFETY: `value` is a handle the caller owns and closes exactly once.
            unsafe { CloseHandle(handle(value)) }
        }

        fn get_last_error(&self) -> u32 {
            // SAFETY: reads the calling thread's last-error value.
            unsafe { GetLastError() }
        }

        fn get_current_process(&self) -> usize {
            // SAFETY: returns a pseudo-handle; no preconditions.
            unsafe { GetCurrentProcess() as usize }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A recording kernel32; `QueryInformationJobObject` reports pids 101 and 202.
    struct Recorded {
        calls: Mutex<Vec<String>>,
        limits: Mutex<Option<Vec<u8>>>,
        set_info_ok: bool,
        in_job: Option<i32>,
        current_in_job: Option<bool>,
    }

    impl Recorded {
        fn new() -> Arc<Recorded> {
            Arc::new(Recorded {
                calls: Mutex::new(Vec::new()),
                limits: Mutex::new(None),
                set_info_ok: true,
                in_job: None,
                current_in_job: None,
            })
        }
        fn push(&self, call: String) {
            self.calls.lock().unwrap().push(call);
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    const JOB: usize = 7;
    const SELF_PROCESS: usize = 99;

    impl Kernel32Job for Recorded {
        fn create_job_object_w(&self) -> usize {
            self.push("CreateJobObjectW".into());
            JOB
        }
        fn set_information_job_object(
            &self,
            _job: usize,
            info_class: i32,
            info: &[u8],
            length: u32,
        ) -> i32 {
            self.push(format!("SetInformationJobObject:{info_class}:{length}"));
            *self.limits.lock().unwrap() = Some(info.to_vec());
            i32::from(self.set_info_ok)
        }
        fn query_information_job_object(
            &self,
            _job: usize,
            _cls: i32,
            info: &mut [u8],
            _length: u32,
        ) -> i32 {
            info[0..4].copy_from_slice(&2u32.to_le_bytes());
            info[4..8].copy_from_slice(&2u32.to_le_bytes());
            info[8..16].copy_from_slice(&101u64.to_le_bytes());
            info[16..24].copy_from_slice(&202u64.to_le_bytes());
            1
        }
        fn open_process(&self, _access: u32, _inherit: i32, pid: u32) -> usize {
            self.push(format!("OpenProcess:{pid}"));
            1000 + pid as usize
        }
        fn assign_process_to_job_object(&self, _job: usize, _process: usize) -> i32 {
            self.push("Assign".into());
            1
        }
        fn is_process_in_job(&self, process: usize, job: Option<usize>, result: &mut [u8]) -> i32 {
            self.push("IsProcessInJob".into());
            let answer = match self.current_in_job {
                Some(inside) => i32::from(inside && process == SELF_PROCESS && job.is_none()),
                None => self.in_job.unwrap_or(1),
            };
            result[..4].copy_from_slice(&answer.to_le_bytes());
            1
        }
        fn terminate_job_object(&self, _job: usize, exit_code: u32) -> i32 {
            self.push(format!("Terminate:{exit_code}"));
            1
        }
        fn close_handle(&self, handle: usize) -> i32 {
            self.push(format!("Close:{handle}"));
            1
        }
        fn get_last_error(&self) -> u32 {
            5
        }
        fn get_current_process(&self) -> usize {
            SELF_PROCESS
        }
    }

    fn with(configure: impl FnOnce(&mut Recorded)) -> Arc<Recorded> {
        let mut kernel = Arc::try_unwrap(Recorded::new()).ok().unwrap();
        configure(&mut kernel);
        Arc::new(kernel)
    }

    #[test]
    fn creates_the_job_with_kill_on_job_close_and_breakaway_ok_in_the_extended_limits() {
        let kernel = Recorded::new();
        WindowsJob::create(kernel.clone()).unwrap();
        assert_eq!(
            kernel.calls(),
            vec![
                "CreateJobObjectW".to_string(),
                format!(
                    "SetInformationJobObject:{JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS}:{EXTENDED_LIMIT_INFORMATION_BYTES}"
                ),
            ]
        );
        let info = kernel.limits.lock().unwrap().clone().unwrap();
        assert_eq!(info.len(), 144);
        let flags = u32::from_le_bytes([
            info[LIMIT_FLAGS_OFFSET],
            info[LIMIT_FLAGS_OFFSET + 1],
            info[LIMIT_FLAGS_OFFSET + 2],
            info[LIMIT_FLAGS_OFFSET + 3],
        ]);
        assert_eq!(flags, 0x2800);
    }

    #[test]
    fn assigns_a_pid_proves_it_with_is_process_in_job_and_closes_the_process_handle() {
        let kernel = Recorded::new();
        WindowsJob::create(kernel.clone())
            .unwrap()
            .assign(42)
            .unwrap();
        assert_eq!(
            kernel.calls()[2..].to_vec(),
            vec!["OpenProcess:42", "Assign", "IsProcessInJob", "Close:1042"]
        );
    }

    #[test]
    fn fails_the_assignment_when_is_process_in_job_reports_the_process_outside_the_job() {
        let kernel = with(|k| k.in_job = Some(0));
        let result = WindowsJob::create(kernel.clone()).unwrap().assign(42);
        assert_eq!(result.unwrap_err().step, "IsProcessInJob");
        assert_eq!(kernel.calls().last().unwrap(), "Close:1042");
    }

    #[test]
    fn closes_the_job_handle_when_the_limits_cannot_be_set() {
        let kernel = with(|k| k.set_info_ok = false);
        let error = WindowsJob::create(kernel.clone()).err().unwrap();
        assert!(
            error
                .to_string()
                .contains("SetInformationJobObject failed (win32 error 5)")
        );
        assert_eq!(
            kernel.calls(),
            vec![
                "CreateJobObjectW".to_string(),
                "SetInformationJobObject:9:144".to_string(),
                "Close:7".to_string()
            ]
        );
    }

    #[test]
    fn lists_member_pids_terminates_and_closes_once() {
        let kernel = Recorded::new();
        let job = WindowsJob::create(kernel.clone()).unwrap();
        assert_eq!(job.member_pids(), vec![101, 202]);
        job.terminate(9);
        job.close();
        job.close();
        job.terminate(1);
        assert_eq!(kernel.calls()[2..].to_vec(), vec!["Terminate:9", "Close:7"]);
    }

    #[test]
    fn reports_whether_this_process_is_in_a_job_from_is_process_in_job_on_the_current_process() {
        let answer = |inside: bool| with(|k| k.current_in_job = Some(inside));
        let inside = answer(true);
        let outside = answer(false);
        assert!(current_process_in_job(Some(&*inside)));
        assert!(!current_process_in_job(Some(&*outside)));
    }

    #[cfg(not(windows))]
    #[test]
    fn has_no_kernel_off_windows() {
        assert!(load_kernel32_job().is_none());
        assert!(!current_process_in_job(None));
    }
}
