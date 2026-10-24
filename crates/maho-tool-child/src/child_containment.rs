//! How a tool child's whole tree is held and ended (senpi#3566, plan todo 4). POSIX: the child leads its own process
//! group, signalled as one. Windows: the child is assigned to its generation's Job Object right after spawn - before it
//! can send `hello` - and membership is confirmed with `IsProcessInJob`; ending the tree terminates the job.
//!
//! senpi `modes/rpc/tool-child/child-containment.ts`.
use std::sync::Arc;

use crate::windows_job::{Kernel32Job, WindowsJob, WindowsJobError, load_kernel32_job};

/// `TOOL_RUNTIME_UNSUPPORTED_HOST` from senpi `cli/tool-runtime.ts` (that module lives above this crate).
pub const TOOL_RUNTIME_UNSUPPORTED_HOST: &str = "tool_runtime_unsupported_host";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildContainmentKind {
    ProcessGroup,
    Job,
}

impl ChildContainmentKind {
    /// The upstream `kind` string.
    pub fn as_str(self) -> &'static str {
        match self {
            ChildContainmentKind::ProcessGroup => "process-group",
            ChildContainmentKind::Job => "job",
        }
    }
}

pub struct ChildContainment {
    pub kind: ChildContainmentKind,
    kill: Box<dyn Fn() + Send + Sync>,
    close: Box<dyn Fn() + Send + Sync>,
    /// Windows: whether a running pid is listed in the job right now (confirmation only, never a refutation).
    is_member: Option<Box<dyn Fn(u64) -> bool + Send + Sync>>,
}

impl ChildContainment {
    /// Ends the child and everything it started (its group, or every job member).
    pub fn kill(&self) {
        (self.kill)()
    }

    /// Releases the handle; the job's kill-on-close ends any member still running.
    pub fn close(&self) {
        (self.close)()
    }

    pub fn has_is_member(&self) -> bool {
        self.is_member.is_some()
    }

    pub fn is_member(&self, pid: u64) -> Option<bool> {
        self.is_member.as_ref().map(|check| check(pid))
    }
}

pub struct ContainmentHost {
    /// A Node platform name: `"win32"` selects the Job Object path, anything else the process-group path.
    pub platform: &'static str,
    pub load_kernel: Box<dyn Fn() -> Option<Arc<dyn Kernel32Job>> + Send + Sync>,
}

impl Default for ContainmentHost {
    fn default() -> Self {
        ContainmentHost {
            platform: if cfg!(windows) { "win32" } else { "posix" },
            load_kernel: Box::new(load_kernel32_job),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {message}")]
pub struct ChildContainmentError {
    pub code: String,
    /// The text after the `code: ` prefix.
    pub message: String,
}

impl ChildContainmentError {
    pub fn new(code: &str, message: &str) -> Self {
        Self {
            code: code.to_string(),
            message: message.to_string(),
        }
    }
}

/// `child_pid` is the spawned child's pid, `None` when the spawn produced none.
pub fn contain_child(
    child_pid: Option<u32>,
    host: &ContainmentHost,
) -> Result<ChildContainment, ChildContainmentError> {
    let Some(pid) = child_pid else {
        return Err(ChildContainmentError::new(
            "tool_child_bootstrap_failed",
            "the tool child has no pid",
        ));
    };
    if host.platform != "win32" {
        return Ok(process_group(pid));
    }
    let Some(kernel) = (host.load_kernel)() else {
        return Err(ChildContainmentError::new(
            TOOL_RUNTIME_UNSUPPORTED_HOST,
            "Windows tool children need bun:ffi for their Job Object",
        ));
    };
    let job = Arc::new(WindowsJob::create(kernel).map_err(job_error)?);
    if let Err(error) = job.assign(pid) {
        job.terminate(1);
        job.close();
        return Err(job_error(error));
    }
    let (kill_job, close_job, member_job) = (job.clone(), job.clone(), job);
    Ok(ChildContainment {
        kind: ChildContainmentKind::Job,
        kill: Box::new(move || kill_job.terminate(1)),
        close: Box::new(move || close_job.close()),
        is_member: Some(Box::new(move |pid| member_job.member_pids().contains(&pid))),
    })
}

fn job_error(error: WindowsJobError) -> ChildContainmentError {
    ChildContainmentError::new(error.code(), &error.to_string())
}

fn process_group(pid: u32) -> ChildContainment {
    ChildContainment {
        kind: ChildContainmentKind::ProcessGroup,
        kill: Box::new(move || kill_process_group(pid)),
        close: Box::new(|| {}),
        is_member: None,
    }
}

#[cfg(unix)]
fn kill_process_group(pid: u32) {
    // `process.kill(-pid, "SIGKILL")`; the group being already gone is not an error.
    let Ok(raw) = i32::try_from(pid) else { return };
    if let Some(group) = rustix::process::Pid::from_raw(raw) {
        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
    }
}

#[cfg(not(unix))]
fn kill_process_group(_pid: u32) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Fake {
        calls: Mutex<Vec<String>>,
        assign: bool,
        in_job: bool,
    }

    impl Fake {
        fn push(&self, call: String) {
            self.calls.lock().unwrap().push(call);
        }
    }

    impl Kernel32Job for Fake {
        fn create_job_object_w(&self) -> usize {
            self.push("create".into());
            1
        }
        fn set_information_job_object(&self, _j: usize, _c: i32, _i: &[u8], _l: u32) -> i32 {
            1
        }
        fn query_information_job_object(&self, _j: usize, _c: i32, _i: &mut [u8], _l: u32) -> i32 {
            0
        }
        fn open_process(&self, _a: u32, _i: i32, pid: u32) -> usize {
            self.push(format!("open:{pid}"));
            2
        }
        fn assign_process_to_job_object(&self, _j: usize, _p: usize) -> i32 {
            self.push("assign".into());
            i32::from(self.assign)
        }
        fn is_process_in_job(&self, _p: usize, _j: Option<usize>, result: &mut [u8]) -> i32 {
            result[..4].copy_from_slice(&i32::from(self.in_job).to_le_bytes());
            1
        }
        fn terminate_job_object(&self, _j: usize, _c: u32) -> i32 {
            self.push("terminate".into());
            1
        }
        fn close_handle(&self, handle: usize) -> i32 {
            self.push(format!("close:{handle}"));
            1
        }
        fn get_last_error(&self) -> u32 {
            5
        }
        fn get_current_process(&self) -> usize {
            0
        }
    }

    fn fake_kernel(assign: bool, in_job: bool) -> Arc<Fake> {
        Arc::new(Fake {
            calls: Mutex::new(Vec::new()),
            assign,
            in_job,
        })
    }

    fn calls(kernel: &Fake) -> Vec<String> {
        kernel.calls.lock().unwrap().clone()
    }

    fn win32_host(kernel: Arc<Fake>) -> ContainmentHost {
        ContainmentHost {
            platform: "win32",
            load_kernel: Box::new(move || Some(kernel.clone() as Arc<dyn Kernel32Job>)),
        }
    }

    #[test]
    fn holds_a_posix_child_by_its_process_group_and_never_loads_a_kernel() {
        let loaded = Arc::new(AtomicBool::new(false));
        let flag = loaded.clone();
        let host = ContainmentHost {
            platform: "darwin",
            load_kernel: Box::new(move || {
                flag.store(true, Ordering::SeqCst);
                None
            }),
        };
        let held = contain_child(Some(4242), &host).unwrap();
        assert_eq!(held.kind.as_str(), "process-group");
        assert!(!loaded.load(Ordering::SeqCst));
        assert!(!held.has_is_member());
    }

    #[test]
    fn puts_a_windows_child_into_a_fresh_job_and_confirms_membership_before_returning() {
        let kernel = fake_kernel(true, true);
        let held = contain_child(Some(4242), &win32_host(kernel.clone())).unwrap();
        assert_eq!(held.kind.as_str(), "job");
        assert_eq!(
            calls(&kernel),
            vec!["create", "open:4242", "assign", "close:2"]
        );
        held.kill();
        held.close();
        assert_eq!(calls(&kernel)[4..].to_vec(), vec!["terminate", "close:1"]);
    }

    #[test]
    fn fails_typed_and_leaves_no_job_behind_when_assignment_or_confirmation_fails() {
        for (assign, in_job) in [(false, true), (true, false)] {
            let kernel = fake_kernel(assign, in_job);
            let error = contain_child(Some(4242), &win32_host(kernel.clone()))
                .err()
                .unwrap();
            assert_eq!(error.code, "tool_child_job_assign_failed");
            let all = calls(&kernel);
            assert_eq!(all[all.len() - 2..].to_vec(), vec!["terminate", "close:1"]);
        }
    }

    #[test]
    fn refuses_a_windows_host_without_a_kernel_with_tool_runtime_unsupported_host() {
        let host = ContainmentHost {
            platform: "win32",
            load_kernel: Box::new(|| None),
        };
        let error = contain_child(Some(4242), &host).err().unwrap();
        assert!(
            error
                .to_string()
                .starts_with("tool_runtime_unsupported_host: ")
        );
    }

    #[test]
    fn refuses_a_child_without_a_pid() {
        let error = contain_child(None, &ContainmentHost::default())
            .err()
            .unwrap();
        assert_eq!(
            error.to_string(),
            "tool_child_bootstrap_failed: the tool child has no pid"
        );
    }
}
