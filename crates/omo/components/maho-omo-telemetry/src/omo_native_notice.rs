use std::{io::Write,os::unix::fs::OpenOptionsExt,path::Path};
pub fn claim_notice(state_dir:&Path)->std::io::Result<bool> {
    std::fs::create_dir_all(state_dir)?;
    match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(state_dir.join("notice-shown")) {
        Ok(mut file)=>{file.write_all(b"shown\n")?;Ok(true)},
        Err(error) if error.kind()==std::io::ErrorKind::AlreadyExists=>Ok(false),
        Err(error)=>Err(error),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn once_per_machine() {let t=tempfile::tempdir().unwrap();assert!(claim_notice(t.path()).unwrap());assert!(!claim_notice(t.path()).unwrap());}
    #[test] fn private_marker() {use std::os::unix::fs::PermissionsExt;let t=tempfile::tempdir().unwrap();claim_notice(t.path()).unwrap();assert_eq!(std::fs::metadata(t.path().join("notice-shown")).unwrap().permissions().mode()&0o777,0o600);}
    #[test] fn blocked_directory_reports_error() {let t=tempfile::tempdir().unwrap();let file=t.path().join("blocked");std::fs::write(&file,"").unwrap();assert!(claim_notice(&file).is_err());}
    #[test] fn stale_marker_suppresses() {let t=tempfile::tempdir().unwrap();std::fs::write(t.path().join("notice-shown"),"shown\n").unwrap();assert!(!claim_notice(t.path()).unwrap());}
    #[test] fn simultaneous_claim_only_one() {let t=tempfile::tempdir().unwrap();let barrier=std::sync::Arc::new(std::sync::Barrier::new(2));let results=std::thread::scope(|scope|{let mut workers=Vec::new();for _ in 0..2 {let barrier=std::sync::Arc::clone(&barrier);let path=t.path();workers.push(scope.spawn(move ||{barrier.wait();claim_notice(path).unwrap()}));}workers.into_iter().map(|w|w.join().unwrap()).collect::<Vec<_>>()});assert_eq!(results.into_iter().filter(|claimed|*claimed).count(),1);}
}
