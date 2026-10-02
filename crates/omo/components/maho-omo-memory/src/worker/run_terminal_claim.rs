use std::{io::Write,path::{Path,PathBuf}};
use serde::{Serialize,Deserialize};
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum RunTerminalClaimKind{Publish,Abandon}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct RunTerminalClaim{pub version:u32,pub kind:RunTerminalClaimKind,pub run_id:String,pub attempt:i64,pub claimant_pid:u32}
pub fn run_terminal_claim_path(run_dir:&Path)->PathBuf{run_dir.join("terminal-claim.json")}
pub fn run_terminal_claim_matches(claim:&RunTerminalClaim,run_id:&str,attempt:i64,kind:Option<RunTerminalClaimKind>)->bool{claim.run_id==run_id&&claim.attempt==attempt&&kind.is_none_or(|kind|claim.kind==kind)}
#[derive(Debug)]pub enum TerminalClaimError{Io(std::io::Error),Invalid(serde_json::Error),InvalidClaim}
impl std::fmt::Display for TerminalClaimError{fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{match self{Self::Io(error)=>error.fmt(f),Self::Invalid(error)=>error.fmt(f),Self::InvalidClaim=>f.write_str("Invalid run terminal claim")}}}
impl std::error::Error for TerminalClaimError{}
pub fn read_run_terminal_claim(run_dir:&Path)->Result<RunTerminalClaim,TerminalClaimError>{let bytes=std::fs::read(run_terminal_claim_path(run_dir)).map_err(TerminalClaimError::Io)?;let claim:RunTerminalClaim=serde_json::from_slice(&bytes).map_err(TerminalClaimError::Invalid)?;if claim.version!=1||claim.claimant_pid==0{return Err(TerminalClaimError::InvalidClaim);}Ok(claim)}
/// Publishes a complete fsynced claim through a filesystem-exclusive hard link.
/// Existing claims are returned unchanged; recovering invalid/dead publishers requires the
/// core's unbounded recovery-lock contract before this can serve the full terminal pipeline.
pub fn claim_run_terminal(run_dir:&Path,run_id:&str,attempt:i64,kind:RunTerminalClaimKind,claimant_pid:u32)->Result<RunTerminalClaim,TerminalClaimError>{
    let requested=RunTerminalClaim{version:1,kind,run_id:run_id.into(),attempt,claimant_pid};if claimant_pid==0{return Err(TerminalClaimError::InvalidClaim);}
    let path=run_terminal_claim_path(run_dir);let candidate=run_dir.join(format!("terminal-claim.json.{}.{}.candidate",std::process::id(),memory_core::support::random::random_uuid()));
    let mut options=std::fs::OpenOptions::new();options.write(true).create_new(true);
    #[cfg(unix)]{use std::os::unix::fs::OpenOptionsExt;options.mode(0o600);}
    let mut file=options.open(&candidate).map_err(TerminalClaimError::Io)?;
    let write=(||{let mut bytes=serde_json::to_vec_pretty(&requested).map_err(TerminalClaimError::Invalid)?;bytes.push(b'\n');file.write_all(&bytes).map_err(TerminalClaimError::Io)?;file.sync_all().map_err(TerminalClaimError::Io)})();drop(file);
    if let Err(error)=write{std::fs::remove_file(&candidate).map_err(TerminalClaimError::Io)?;return Err(error);}
    let linked=std::fs::hard_link(&candidate,&path);std::fs::remove_file(&candidate).map_err(TerminalClaimError::Io)?;
    match linked{Ok(())=>{std::fs::File::open(run_dir).and_then(|dir|dir.sync_all()).map_err(TerminalClaimError::Io)?;Ok(requested)},Err(error) if error.kind()==std::io::ErrorKind::AlreadyExists=>read_run_terminal_claim(run_dir),Err(error)=>Err(TerminalClaimError::Io(error))}
}
#[cfg(test)]mod tests{
    use super::*;
    #[test]fn first_claim_wins_and_no_candidates_remain(){let root=tempfile::tempdir().unwrap();let first=claim_run_terminal(root.path(),"run",1,RunTerminalClaimKind::Publish,std::process::id()).unwrap();let competing=claim_run_terminal(root.path(),"run",1,RunTerminalClaimKind::Abandon,std::process::id()).unwrap();assert_eq!(first,competing);assert!(run_terminal_claim_matches(&first,"run",1,Some(RunTerminalClaimKind::Publish)));assert!(!run_terminal_claim_matches(&first,"run",2,None));assert_eq!(std::fs::read_dir(root.path()).unwrap().count(),1);}
    #[test]fn concurrent_claims_observe_one_complete_winner(){let root=tempfile::tempdir().unwrap();let barrier=std::sync::Arc::new(std::sync::Barrier::new(8));let winners=std::thread::scope(|scope|{let handles:Vec<_>=(0..8).map(|index|{let barrier=barrier.clone();let path=root.path();scope.spawn(move||{barrier.wait();claim_run_terminal(path,"run",index,RunTerminalClaimKind::Publish,std::process::id()).unwrap()})}).collect();handles.into_iter().map(|handle|handle.join().unwrap()).collect::<Vec<_>>()});assert!(winners.iter().all(|claim|claim==&winners[0]));assert_eq!(read_run_terminal_claim(root.path()).unwrap(),winners[0]);assert_eq!(std::fs::read_dir(root.path()).unwrap().count(),1);}
    #[test]fn invalid_claim_is_not_authorized(){let root=tempfile::tempdir().unwrap();std::fs::write(run_terminal_claim_path(root.path()),r#"{"version":1,"kind":"publish","runId":"run","attempt":1,"claimantPid":0}"#).unwrap();assert!(matches!(read_run_terminal_claim(root.path()),Err(TerminalClaimError::InvalidClaim)));assert!(claim_run_terminal(root.path(),"run",1,RunTerminalClaimKind::Abandon,std::process::id()).is_err());}
}
