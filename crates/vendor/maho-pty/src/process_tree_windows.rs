//! Windows process-tree termination.
//!
//! A PTY child on Windows is rarely the process doing the work: Git Bash's `bash.exe` is a launcher
//! for `usr\bin\bash.exe`, and a shell runs each command as a separate Windows process. Terminating
//! only the direct child leaves those descendants attached to the pseudoconsole, and the ConPTY
//! output pipe reaches EOF only after every console client is gone, so the reader (and with it the
//! exit wait) would block until a grandchild such as `sleep 30` exits on its own.

use std::ffi::c_void;

type Handle = *mut c_void;

const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
const PROCESS_TERMINATE: u32 = 0x0001;
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
/// Passes over the process table; each pass adopts children spawned since the previous one. The
/// cap only bounds a tree that keeps spawning faster than it is terminated.
const MAX_PASSES: usize = 16;

#[repr(C)]
struct ProcessEntry32W {
    dw_size: u32,
    cnt_usage: u32,
    th32_process_id: u32,
    th32_default_heap_id: usize,
    th32_module_id: u32,
    cnt_threads: u32,
    th32_parent_process_id: u32,
    pc_pri_class_base: i32,
    dw_flags: u32,
    sz_exe_file: [u16; 260],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FileTime {
    low: u32,
    high: u32,
}

unsafe extern "system" {
    fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> Handle;
    fn Process32FirstW(snapshot: Handle, entry: *mut ProcessEntry32W) -> i32;
    fn Process32NextW(snapshot: Handle, entry: *mut ProcessEntry32W) -> i32;
    fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> Handle;
    fn TerminateProcess(process: Handle, exit_code: u32) -> i32;
    fn GetProcessTimes(
        process: Handle,
        creation: *mut FileTime,
        exit: *mut FileTime,
        kernel: *mut FileTime,
        user: *mut FileTime,
    ) -> i32;
    fn CloseHandle(object: Handle) -> i32;
}

/// An open process handle; holding it keeps the pid from being reused while the tree is walked.
struct Process {
    pid: u32,
    handle: Handle,
    created: u64,
}

impl Process {
    fn open(pid: u32) -> Option<Self> {
        // SAFETY: OpenProcess takes plain integers; a null handle is checked before any use.
        let handle = unsafe {
            OpenProcess(
                PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            )
        };
        if handle.is_null() {
            return None;
        }
        let mut times = [FileTime::default(); 4];
        let [creation, exit, kernel, user] = &mut times;
        // SAFETY: `handle` is a live process handle and every out-pointer outlives the call.
        let queried = unsafe { GetProcessTimes(handle, creation, exit, kernel, user) };
        let created = (u64::from(times[0].high) << 32) | u64::from(times[0].low);
        let process = Self {
            pid,
            handle,
            created,
        };
        (queried != 0).then_some(process)
    }

    fn terminate(&self) -> bool {
        // SAFETY: `handle` is a live process handle opened with PROCESS_TERMINATE.
        unsafe { TerminateProcess(self.handle, 1) != 0 }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // SAFETY: `handle` came from OpenProcess and is closed exactly once, here.
        unsafe { CloseHandle(self.handle) };
    }
}

/// `(pid, parent pid)` for every process in the system.
pub(crate) fn process_table() -> Vec<(u32, u32)> {
    // SAFETY: CreateToolhelp32Snapshot takes plain integers; the result is checked before use.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    // INVALID_HANDLE_VALUE is -1.
    if snapshot.is_null() || snapshot as isize == -1 {
        return Vec::new();
    }
    let mut table = Vec::new();
    // SAFETY: ProcessEntry32W is plain data; all-zero is a valid value before dw_size is set.
    let mut entry: ProcessEntry32W = unsafe { std::mem::zeroed() };
    entry.dw_size = std::mem::size_of::<ProcessEntry32W>() as u32;
    // SAFETY: `snapshot` is a live snapshot handle and `entry` is a sized, writable entry.
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    while more {
        table.push((entry.th32_process_id, entry.th32_parent_process_id));
        // SAFETY: as above.
        more = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }
    // SAFETY: `snapshot` came from CreateToolhelp32Snapshot and is closed exactly once.
    unsafe { CloseHandle(snapshot) };
    table
}

/// `taskkill /T /F` without the subprocess: terminate `root`, then every descendant. Returns
/// whether `root` itself was terminated; descendants are terminated even when `root` had already
/// exited, since they may still hold the console open.
///
/// A process counts as a child only if it was created no earlier than its parent, so a stale
/// parent pid that now names a reused process never pulls an unrelated process into the tree.
pub(crate) fn terminate_process_tree(root: u32) -> bool {
    let Some(root) = Process::open(root) else {
        return false;
    };
    let root_terminated = root.terminate();
    let mut tree = vec![root];
    for _ in 0..MAX_PASSES {
        let mut adopted = Vec::new();
        for (pid, parent) in process_table() {
            if tree.iter().chain(&adopted).any(|known: &Process| known.pid == pid) {
                continue;
            }
            let Some(parent) = tree.iter().chain(&adopted).find(|known| known.pid == parent) else {
                continue;
            };
            let parent_created = parent.created;
            if let Some(child) = Process::open(pid).filter(|child| child.created >= parent_created) {
                child.terminate();
                adopted.push(child);
            }
        }
        if adopted.is_empty() {
            break;
        }
        tree.extend(adopted);
    }
    root_terminated
}
