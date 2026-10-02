use std::fs;
use rustix::process::{Pid,WaitId,WaitIdOptions,WaitOptions,waitid,waitpid};

pub trait ChildReaperSyscalls {
    fn list_direct_children(&mut self) -> Vec<i32>;
    fn is_waitable(&mut self,pid:i32) -> bool;
    fn reap_exited(&mut self,pid:i32) -> bool;
    fn describe(&mut self,pid:i32) -> String;
}
pub struct LinuxChildReaperSyscalls;
fn linux_stat(pid:i32) -> Option<String> { fs::read_to_string(format!("/proc/{pid}/stat")).ok() }
pub fn parent_of_stat(stat:&str) -> Option<i32> { stat.rsplit_once(") ")?.1.split_whitespace().nth(1)?.parse().ok() }
pub fn name_of_stat(stat:&str) -> &str {
    match (stat.find('('),stat.rfind(')')) { (Some(open),Some(close)) if close>open => &stat[open+1..close],_ => "" }
}
impl ChildReaperSyscalls for LinuxChildReaperSyscalls {
    fn list_direct_children(&mut self) -> Vec<i32> {
        let Ok(entries)=fs::read_dir("/proc") else { return Vec::new(); };
        let own=i32::try_from(std::process::id()).unwrap_or(i32::MAX);
        entries.filter_map(Result::ok).filter_map(|entry| entry.file_name().to_str()?.parse::<i32>().ok()).filter(|pid| *pid>0 && linux_stat(*pid).as_deref().and_then(parent_of_stat)==Some(own)).take(4096).collect()
    }
    fn is_waitable(&mut self,pid:i32) -> bool {
        let Some(pid)=Pid::from_raw(pid) else { return false; };
        matches!(waitid(WaitId::Pid(pid),WaitIdOptions::EXITED|WaitIdOptions::NOHANG|WaitIdOptions::NOWAIT),Ok(Some(_)))
    }
    fn reap_exited(&mut self,pid:i32) -> bool {
        assert!(pid>0,"child reaper refuses to wait on pid {pid}");
        let Some(pid)=Pid::from_raw(pid) else { return false; };
        matches!(waitpid(Some(pid),WaitOptions::NOHANG),Ok(Some((found,_))) if found==pid)
    }
    fn describe(&mut self,pid:i32) -> String { linux_stat(pid).as_deref().map(name_of_stat).unwrap_or("").into() }
}
pub fn load_child_reaper_syscalls(platform:&str) -> Option<LinuxChildReaperSyscalls> {
    (platform=="linux").then_some(LinuxChildReaperSyscalls)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn stat_names_can_contain_spaces_and_parentheses() { let stat="42 (a (b) c) Z 7 0 0";assert_eq!(parent_of_stat(stat),Some(7));assert_eq!(name_of_stat(stat),"a (b) c"); }
    #[test] #[should_panic(expected="child reaper refuses to wait on pid -1")] fn wildcard_is_never_waited_on() { LinuxChildReaperSyscalls.reap_exited(-1); }
    #[test] fn current_process_is_not_waitable() { let mut sys=LinuxChildReaperSyscalls;assert!(!sys.is_waitable(i32::try_from(std::process::id()).unwrap())); }
    #[test] fn unsupported_platform_does_not_load() { assert!(load_child_reaper_syscalls("win32").is_none()); }
}
