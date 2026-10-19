use std::path::{Path,PathBuf};
use std::io::Write;
use serde::{Serialize,Deserialize};
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct LeaseRecord {pub pid:f64,pub started_at_ms:f64}
#[derive(Debug,PartialEq)]
pub enum AcquireTerminalLeaseResult {Acquired {path:PathBuf,pid:f64},Held {holder:LeaseRecord}}
enum LeaseRead {Missing,Unparseable,Record(LeaseRecord)}
fn read_lease(path:&Path)->std::io::Result<LeaseRead> {let raw=match std::fs::read(path) {Ok(raw)=>raw,Err(error) if error.kind()==std::io::ErrorKind::NotFound=>return Ok(LeaseRead::Missing),Err(error)=>return Err(error)};Ok(match serde_json::from_slice::<LeaseRecord>(&raw) {Ok(record) if record.pid.is_finite()&&record.started_at_ms.is_finite()=>LeaseRead::Record(record),_=>LeaseRead::Unparseable})}
fn unlink_if_present(path:&Path)->std::io::Result<()> {match std::fs::remove_file(path) {Err(error) if error.kind()==std::io::ErrorKind::NotFound=>Ok(()),result=>result}}
pub fn acquire_terminal_lease(dir:&Path,encoded_session_id:&str,pid:f64,started_at_ms:f64,mut is_process_alive:impl FnMut(f64)->std::io::Result<bool>)->std::io::Result<AcquireTerminalLeaseResult> {
    std::fs::create_dir_all(dir)?;let path=dir.join(format!("{encoded_session_id}.lease"));let record=LeaseRecord {pid,started_at_ms};
    for retry in [true,false] {
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file)=>{file.write_all(&serde_json::to_vec(&record).map_err(std::io::Error::other)?)?;return Ok(AcquireTerminalLeaseResult::Acquired {path,pid});},
            Err(error) if error.kind()==std::io::ErrorKind::AlreadyExists=> {
                let existing=read_lease(&path)?;
                let reclaimable=match &existing {LeaseRead::Missing|LeaseRead::Unparseable=>true,LeaseRead::Record(record)=>match is_process_alive(record.pid) {Ok(alive)=>!alive,Err(error) if error.kind()==std::io::ErrorKind::PermissionDenied=>false,Err(error) if error.kind()==std::io::ErrorKind::NotFound=>true,Err(error)=>return Err(error)}};
                if reclaimable&&retry {unlink_if_present(&path)?;continue;}
                return match existing {LeaseRead::Record(holder)=>Ok(AcquireTerminalLeaseResult::Held {holder}),_=>Err(error)};
            },Err(error)=>return Err(error),
        }
    }
    unreachable!("second exclusive acquisition always returns")
}
pub fn release_terminal_lease(path:&Path,pid:f64)->std::io::Result<()> {if let LeaseRead::Record(record)=read_lease(path)? && record.pid==pid {unlink_if_present(path)?;}Ok(())}
#[cfg(unix)]
pub fn probe_alive(pid:f64)->std::io::Result<bool> {
    match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32),None) {
        Ok(())|Err(nix::errno::Errno::EPERM)=>Ok(true),Err(nix::errno::Errno::ESRCH)=>Ok(false),Err(error)=>Err(std::io::Error::from_raw_os_error(error as i32)),
    }
}
#[cfg(windows)]
pub fn probe_alive(pid:f64)->std::io::Result<bool> {
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;
    const ERROR_INVALID_PARAMETER: u32 = 87;

    extern "system" {
        fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> *mut std::ffi::c_void;
        fn GetExitCodeProcess(process: *mut std::ffi::c_void, exit_code: *mut u32) -> i32;
        fn CloseHandle(object: *mut std::ffi::c_void) -> i32;
        fn GetLastError() -> u32;
    }

    if !pid.is_finite() || pid <= 0.0 || pid > u32::MAX as f64 {
        return Ok(false);
    }
    let pid = pid as u32;
    // SAFETY: OpenProcess takes integer arguments; a null handle is checked before use.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        let error = unsafe { GetLastError() };
        if error == ERROR_INVALID_PARAMETER {
            return Ok(false);
        }
        if error == 5 {
            return Ok(true);
        }
        return Ok(false);
    }
    let mut exit_code = 0u32;
    // SAFETY: handle is valid and exit_code is valid pointer.
    let queried = unsafe { GetExitCodeProcess(handle, &mut exit_code) };
    // SAFETY: handle came from OpenProcess and is closed once.
    unsafe { CloseHandle(handle) };
    if queried != 0 && exit_code == STILL_ACTIVE {
        Ok(true)
    } else {
        Ok(false)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn default_probe_observes_current_process()->std::io::Result<()> {assert!(probe_alive(f64::from(std::process::id()))?);Ok(())}
    #[test] fn first_acquire_writes_pid()->std::io::Result<()> {let dir=tempfile::tempdir()?;let path=dir.path().join("nested/s.lease");assert_eq!(acquire_terminal_lease(&dir.path().join("nested"),"s",12.0,99.0,|_|Ok(true))?,AcquireTerminalLeaseResult::Acquired {path:path.clone(),pid:12.0});assert!(matches!(read_lease(&path)?,LeaseRead::Record(LeaseRecord {pid:12.0,started_at_ms:99.0})));Ok(())}
    #[test] fn second_acquire_retains_live_holder()->std::io::Result<()> {let dir=tempfile::tempdir()?;acquire_terminal_lease(dir.path(),"s",12.0,99.0,|_|Ok(true))?;assert_eq!(acquire_terminal_lease(dir.path(),"s",13.0,100.0,|_|Ok(true))?,AcquireTerminalLeaseResult::Held {holder:LeaseRecord {pid:12.0,started_at_ms:99.0}});Ok(())}
    #[test] fn reclaims_dead_pid()->std::io::Result<()> {let dir=tempfile::tempdir()?;acquire_terminal_lease(dir.path(),"s",12.0,99.0,|_|Ok(true))?;assert!(matches!(acquire_terminal_lease(dir.path(),"s",13.0,100.0,|_|Ok(false))?,AcquireTerminalLeaseResult::Acquired {pid:13.0,..}));Ok(())}
    #[test] fn old_live_holder_is_not_reclaimed()->std::io::Result<()> {let dir=tempfile::tempdir()?;acquire_terminal_lease(dir.path(),"s",12.0,1.0,|_|Ok(true))?;assert!(matches!(acquire_terminal_lease(dir.path(),"s",13.0,660001.0,|_|Ok(true))?,AcquireTerminalLeaseResult::Held {..}));Ok(())}
    #[test] fn eperm_is_alive()->std::io::Result<()> {let dir=tempfile::tempdir()?;acquire_terminal_lease(dir.path(),"s",12.0,1.0,|_|Ok(true))?;assert!(matches!(acquire_terminal_lease(dir.path(),"s",13.0,2.0,|_|Err(std::io::ErrorKind::PermissionDenied.into()))?,AcquireTerminalLeaseResult::Held {..}));Ok(())}
    #[test] fn release_matches_pid_only()->std::io::Result<()> {let dir=tempfile::tempdir()?;acquire_terminal_lease(dir.path(),"s",12.0,1.0,|_|Ok(true))?;let path=dir.path().join("s.lease");release_terminal_lease(&path,13.0)?;assert!(path.exists());release_terminal_lease(&path,12.0)?;assert!(!path.exists());Ok(())}
    #[test] fn corrupt_lease_reclaimed()->std::io::Result<()> {let dir=tempfile::tempdir()?;std::fs::write(dir.path().join("s.lease"),"not-json{{{")?;assert!(matches!(acquire_terminal_lease(dir.path(),"s",13.0,2.0,|_|Ok(true))?,AcquireTerminalLeaseResult::Acquired {..}));Ok(())}
}
