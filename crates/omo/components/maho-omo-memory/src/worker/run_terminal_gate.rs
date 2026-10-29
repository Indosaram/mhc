use std::path::Path;
use memory_core::locks::{LockRecord,CreateLockRecordOptions,create_lock_record};

/// The native lock owner must implement an unbounded wait, not a finite timeout.
pub fn with_run_terminal_gate<'a,T>(run_dir:&Path,run_id:&str,operation:impl FnOnce()->Result<T,String>+'a,lock:impl FnOnce(&Path,&LockRecord,Box<dyn FnOnce()->Result<T,String>+'a>)->Result<T,String>)->Result<T,String> {
    let record=create_lock_record("reflection-finalize",CreateLockRecordOptions{run_id:Some(run_id.into())}).map_err(|error|error.to_string())?;
    lock(&run_dir.join("terminalization.lock"),&record,Box::new(operation))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gate_supplies_source_path_purpose_and_run_identity() {
        let root=tempfile::tempdir().unwrap();
        let result=with_run_terminal_gate(root.path(),"run",||Ok(7),|path,record,operation| {
            assert_eq!(path,root.path().join("terminalization.lock"));
            assert_eq!(record.purpose,"reflection-finalize");assert_eq!(record.run_id.as_deref(),Some("run"));
            operation()
        }).unwrap();assert_eq!(result,7);
    }
    #[test]
    fn lock_error_prevents_operation() {
        assert_eq!(with_run_terminal_gate::<()>(Path::new("unused"),"run",||panic!("gate failed"),|_,_,_|Err("lock failed".into())).unwrap_err(),"lock failed");
    }
}
