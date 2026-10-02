use std::fs::{self,OpenOptions};
use std::io::Write;
use std::path::{Path,PathBuf};
use serde_json::Value;
pub const HOST_CRASH_RECORD_LIMIT:usize = 50;
pub fn host_crash_record_file(daemon_dir:&Path) -> PathBuf { daemon_dir.join("crashes.jsonl") }
pub fn read_host_crash_records(daemon_dir:&Path) -> Vec<Value> {
    let Ok(raw) = fs::read_to_string(host_crash_record_file(daemon_dir)) else { return Vec::new(); };
    raw.lines().filter_map(|line| serde_json::from_str::<Value>(line).ok()).filter(|value| value.is_object() && value.get("at").is_some_and(Value::is_string)).collect()
}
pub fn record_host_crash(daemon_dir:&Path,record:&Value) {
    if let Err(error) = append_and_prune(daemon_dir,record) { eprintln!("RPC crash evidence write failed: {error}"); }
}
fn append_and_prune(daemon_dir:&Path,record:&Value) -> Result<(),std::io::Error> {
    #[cfg(unix)] use std::os::unix::fs::{DirBuilderExt,OpenOptionsExt};
    let mut directory = fs::DirBuilder::new();directory.recursive(true);
    #[cfg(unix)] directory.mode(0o700);
    directory.create(daemon_dir)?;
    let path = host_crash_record_file(daemon_dir);
    let mut options = OpenOptions::new();options.create(true).append(true);
    #[cfg(unix)] options.mode(0o600);
    let mut file=options.open(&path)?;
    writeln!(file,"{record}")?;
    let records=read_host_crash_records(daemon_dir);
    if records.len() > HOST_CRASH_RECORD_LIMIT {
        let mut options=OpenOptions::new();options.write(true).truncate(true).create(true);
        #[cfg(unix)] options.mode(0o600);
        let mut file=options.open(path)?;
        for record in records.iter().skip(records.len()-HOST_CRASH_RECORD_LIMIT) { writeln!(file,"{record}")?; }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_child_exit::note_child_exit;
    use serde_json::json;
    #[test] fn first_crash_survives_replacement_state() { let dir=tempfile::tempdir().unwrap();note_child_exit(dir.path(),None,Some("SIGBUS"),0,3_061_000);fs::write(dir.path().join("settings.json"),"{}").unwrap();let records=read_host_crash_records(dir.path());assert_eq!(records[0]["uptimeMs"],3_061_000);assert_eq!(records[0]["signal"],"SIGBUS"); }
    #[test] fn repeated_crashes_accumulate_in_order() { let dir=tempfile::tempdir().unwrap();note_child_exit(dir.path(),None,Some("SIGSEGV"),0,5000);note_child_exit(dir.path(),None,Some("SIGBUS"),0,9000);let records=read_host_crash_records(dir.path());assert_eq!(records.len(),2);assert_eq!(records[0]["signal"],"SIGSEGV");assert_eq!(records[1]["signal"],"SIGBUS"); }
    #[test] fn nonzero_exit_records_code_not_signal() { let dir=tempfile::tempdir().unwrap();note_child_exit(dir.path(),Some(1),None,0,1500);let records=read_host_crash_records(dir.path());assert_eq!(records[0]["code"],1);assert!(records[0].get("signal").is_none()); }
    #[test] fn clean_idle_exit_writes_nothing() { let dir=tempfile::tempdir().unwrap();note_child_exit(dir.path(),Some(0),None,0,900000);assert!(read_host_crash_records(dir.path()).is_empty());assert!(!host_crash_record_file(dir.path()).exists()); }
    #[test] fn crash_loop_keeps_newest_fifty() { let dir=tempfile::tempdir().unwrap();for i in 0..60 { record_host_crash(dir.path(),&json!({"at":"2026-10-01T00:00:00.000Z","uptimeMs":i})); }let records=read_host_crash_records(dir.path());assert_eq!(records.len(),50);assert_eq!(records[0]["uptimeMs"],10);assert_eq!(records[49]["uptimeMs"],59); }
    #[test] fn unusable_directory_does_not_interrupt_shutdown() { let dir=tempfile::tempdir().unwrap();let file=dir.path().join("file");fs::write(&file,"x").unwrap();record_host_crash(&file.join("daemon"),&json!({"at":"fixed"})); }
    #[test] fn torn_final_line_is_skipped() { let dir=tempfile::tempdir().unwrap();record_host_crash(dir.path(),&json!({"at":"fixed","uptimeMs":42}));let mut file=OpenOptions::new().append(true).open(host_crash_record_file(dir.path())).unwrap();write!(file,"{{\"at\":\"tru").unwrap();assert_eq!(read_host_crash_records(dir.path()).len(),1); }
}
