#[cfg(target_os="linux")]
use std::fs;
#[cfg(unix)]
use rustix::process::{Pid,WaitId,WaitIdOptions,WaitOptions,waitid,waitpid};

pub trait ChildReaperSyscalls {
    fn list_direct_children(&mut self) -> Vec<i32>;
    fn is_waitable(&mut self,pid:i32) -> bool;
    fn reap_exited(&mut self,pid:i32) -> bool;
    fn describe(&mut self,pid:i32) -> String;
}
/// Kernel-backed syscalls: a `/proc` ppid scan on linux, libproc on darwin (senpi `loadChildReaperSyscalls`).
pub struct KernelChildReaperSyscalls;
#[cfg(target_os="linux")]
fn linux_stat(pid:i32) -> Option<String> { fs::read_to_string(format!("/proc/{pid}/stat")).ok() }
pub fn parent_of_stat(stat:&str) -> Option<i32> { stat.rsplit_once(") ")?.1.split_whitespace().nth(1)?.parse().ok() }
pub fn name_of_stat(stat:&str) -> &str {
    match (stat.find('('),stat.rfind(')')) { (Some(open),Some(close)) if close>open => &stat[open+1..close],_ => "" }
}
impl ChildReaperSyscalls for KernelChildReaperSyscalls {
    #[cfg(target_os="linux")]
    fn list_direct_children(&mut self) -> Vec<i32> {
        let Ok(entries)=fs::read_dir("/proc") else { return Vec::new(); };
        let own=i32::try_from(std::process::id()).unwrap_or(i32::MAX);
        entries.filter_map(Result::ok).filter_map(|entry| entry.file_name().to_str()?.parse::<i32>().ok()).filter(|pid| *pid>0 && linux_stat(*pid).as_deref().and_then(parent_of_stat)==Some(own)).take(4096).collect()
    }
    #[cfg(not(unix))]
    fn list_direct_children(&mut self) -> Vec<i32> { Vec::new() }
    /// darwin: libproc `proc_listchildpids`, which lists zombies too.
    #[cfg(target_os="macos")]
    fn list_direct_children(&mut self) -> Vec<i32> {
        libproc::processes::pids_by_type(libproc::processes::ProcFilter::ByParentProcess{ppid:std::process::id()}).map(|pids|pids.into_iter().filter_map(|pid|i32::try_from(pid).ok()).filter(|pid|*pid>0).take(4096).collect()).unwrap_or_default()
    }
    #[cfg(unix)]
    fn is_waitable(&mut self,pid:i32) -> bool {
        let Some(pid)=Pid::from_raw(pid) else { return false; };
        matches!(waitid(WaitId::Pid(pid),WaitIdOptions::EXITED|WaitIdOptions::NOHANG|WaitIdOptions::NOWAIT),Ok(Some(_)))
    }
    #[cfg(unix)]
    fn reap_exited(&mut self,pid:i32) -> bool {
        assert!(pid>0,"child reaper refuses to wait on pid {pid}");
        let Some(pid)=Pid::from_raw(pid) else { return false; };
        matches!(waitpid(Some(pid),WaitOptions::NOHANG),Ok(Some((found,_))) if found==pid)
    }
    #[cfg(not(unix))]
    fn is_waitable(&mut self,_pid:i32) -> bool { false }
    #[cfg(not(unix))]
    fn reap_exited(&mut self,_pid:i32) -> bool { false }
    #[cfg(target_os="linux")]
    fn describe(&mut self,pid:i32) -> String { linux_stat(pid).as_deref().map(name_of_stat).unwrap_or("").into() }
    #[cfg(not(unix))]
    fn describe(&mut self,_pid:i32) -> String { String::new() }
    /// darwin: libproc `proc_name`; "" once the kernel dropped the process.
    #[cfg(target_os="macos")]
    fn describe(&mut self,pid:i32) -> String { libproc::proc_pid::name(pid).unwrap_or_default() }
}
/// The syscalls for the platform this binary runs on, or nothing elsewhere (win32).
pub fn load_child_reaper_syscalls(platform:&str) -> Option<KernelChildReaperSyscalls> {
    let supported=if cfg!(target_os="macos"){matches!(platform,"darwin"|"macos")}else{platform=="linux"};
    supported.then_some(KernelChildReaperSyscalls)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn stat_names_can_contain_spaces_and_parentheses() { let stat="42 (a (b) c) Z 7 0 0";assert_eq!(parent_of_stat(stat),Some(7));assert_eq!(name_of_stat(stat),"a (b) c"); }
    #[test] #[should_panic(expected="child reaper refuses to wait on pid -1")] fn wildcard_is_never_waited_on() { KernelChildReaperSyscalls.reap_exited(-1); }
    #[test] fn current_process_is_not_waitable() { let mut sys=KernelChildReaperSyscalls;assert!(!sys.is_waitable(i32::try_from(std::process::id()).unwrap())); }
    #[cfg(unix)]
    #[test] fn real_child_is_listed_waitable_only_after_exit_and_reaped_once() {
        let platform=if cfg!(target_os="macos"){"darwin"}else{"linux"};let mut sys=load_child_reaper_syscalls(platform).expect("own platform syscalls");
        let mut child=std::process::Command::new("/bin/cat").stdin(std::process::Stdio::piped()).spawn().unwrap();let pid=i32::try_from(child.id()).unwrap();
        assert!(sys.list_direct_children().contains(&pid));assert_eq!(sys.describe(pid),"cat");assert!(!sys.is_waitable(pid));
        drop(child.stdin.take());
        waitid(WaitId::Pid(Pid::from_child(&child)),WaitIdOptions::EXITED|WaitIdOptions::NOWAIT).unwrap();
        assert!(sys.list_direct_children().contains(&pid));assert!(sys.is_waitable(pid));assert!(sys.reap_exited(pid));
        assert!(!sys.list_direct_children().contains(&pid));assert!(!sys.reap_exited(pid));assert!(child.try_wait().is_err());
    }
    #[test] fn unsupported_platform_does_not_load() { assert!(load_child_reaper_syscalls("win32").is_none()); }
}
