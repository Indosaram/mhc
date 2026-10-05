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
/// Recovery uses the caller's unbounded cross-process recovery lock. Inspection
/// and the recovery hook occur before the gate, then raw bytes are rechecked
/// under it so a replacement claim cannot be removed by a stale observer.
pub fn claim_run_terminal_with_recovery(
    run_dir:&Path,requested:&RunTerminalClaim,
    liveness:impl Fn(u32)->memory_core::locks::ProcessLiveness,
    mut before_recovery:impl FnMut(&str),
    mut gate:impl FnMut(&Path,&mut dyn FnMut()->Result<Option<RunTerminalClaim>,TerminalClaimError>)->Result<Option<RunTerminalClaim>,TerminalClaimError>,
)->Result<RunTerminalClaim,TerminalClaimError>{
    if requested.version!=1||requested.claimant_pid==0{return Err(TerminalClaimError::InvalidClaim);}
    let run_id=requested.run_id.as_str();let attempt=requested.attempt;let kind=requested.kind;let claimant_pid=requested.claimant_pid;
    let path=run_terminal_claim_path(run_dir);let mut discarded=0;
    loop{
        let result=claim_run_terminal(run_dir,run_id,attempt,kind,claimant_pid);
        if let Ok(claim)=&result{
            if run_terminal_claim_matches(claim,run_id,attempt,Some(kind))||claim.kind!=RunTerminalClaimKind::Publish||liveness(claim.claimant_pid)!=memory_core::locks::ProcessLiveness::Dead{return Ok(claim.clone());}
        }else if matches!(&result,Err(TerminalClaimError::Io(error)) if error.kind()==std::io::ErrorKind::NotFound){continue;}
        else if !matches!(&result,Err(TerminalClaimError::Invalid(_)|TerminalClaimError::InvalidClaim)){return result;}
        let inspected=match std::fs::read(&path){Ok(raw)=>raw,Err(error) if error.kind()==std::io::ErrorKind::NotFound=>continue,Err(error)=>return Err(TerminalClaimError::Io(error))};
        let snapshot=serde_json::from_slice::<RunTerminalClaim>(&inspected).ok().filter(|claim|claim.version==1&&claim.claimant_pid!=0);
        if snapshot.as_ref()!=result.as_ref().ok(){continue;}
        let invalid=result.is_err();if invalid{if discarded>0{return Err(TerminalClaimError::InvalidClaim);}discarded+=1;}
        before_recovery(if invalid{"invalid"}else{"dead-publish"});
        let recovered=gate(&run_dir.join("terminal-claim-recovery.lock"),&mut ||{
            let raw=match std::fs::read(&path){Ok(raw)=>raw,Err(error) if error.kind()==std::io::ErrorKind::NotFound=>return Ok(None),Err(error)=>return Err(TerminalClaimError::Io(error))};
            let current=match read_run_terminal_claim(run_dir){Ok(claim)=>Some(claim),Err(TerminalClaimError::Invalid(_)|TerminalClaimError::InvalidClaim)=>None,Err(error)=>return Err(error)};
            if raw!=inspected{return Ok(current);}
            if invalid{if current.is_some(){return Ok(current);}}else{
                let Some(claim)=current else{return Ok(None);};
                if claim.kind!=RunTerminalClaimKind::Publish||liveness(claim.claimant_pid)!=memory_core::locks::ProcessLiveness::Dead{return Ok(Some(claim));}
            }
            match std::fs::remove_file(&path){Ok(())=>{},Err(error) if error.kind()==std::io::ErrorKind::NotFound=>{},Err(error)=>return Err(TerminalClaimError::Io(error))};
            std::fs::File::open(run_dir).and_then(|directory|directory.sync_all()).map_err(TerminalClaimError::Io)?;Ok(None)
        })?;if let Some(claim)=recovered{return Ok(claim);}
    }
}
#[cfg(test)]mod tests{
    use super::*;
    #[test]fn empty_crash_claim_recovers_under_injected_gate(){
        let root=tempfile::tempdir().unwrap();std::fs::write(run_terminal_claim_path(root.path()),"").unwrap();let mut gated=false;
        let result=claim_run_terminal_with_recovery(root.path(),&RunTerminalClaim{version:1,run_id:"run".into(),attempt:1,kind:RunTerminalClaimKind::Abandon,claimant_pid:1111},|_|memory_core::locks::ProcessLiveness::Dead,|reason|assert_eq!(reason,"invalid"),|path,operation|{assert_eq!(path,root.path().join("terminal-claim-recovery.lock"));gated=true;operation()}).unwrap();
        assert!(gated);assert_eq!(result.kind,RunTerminalClaimKind::Abandon);assert_eq!(read_run_terminal_claim(root.path()).unwrap(),result);
    }
    fn both_inspected(signal:&(std::sync::Mutex<usize>,std::sync::Condvar)){
        let mut count=signal.0.lock().unwrap();*count+=1;signal.1.notify_all();
        let (count,timeout)=signal.1.wait_timeout_while(count,std::time::Duration::from_secs(5),|count|*count<2).unwrap();
        assert!(*count>=2&&!timeout.timed_out(),"claimants did not both inspect stale state");
    }
    fn recovery_race(initial:&str){
        let root=tempfile::tempdir().unwrap();std::fs::write(run_terminal_claim_path(root.path()),initial).unwrap();let inspected=(std::sync::Mutex::new(0),std::sync::Condvar::new());let finished=(std::sync::Mutex::new(false),std::sync::Condvar::new());let gate=std::sync::Mutex::new(());
        let claims=std::thread::scope(|scope|{
            let abandon=scope.spawn(||{let result=claim_run_terminal_with_recovery(root.path(),&RunTerminalClaim{version:1,run_id:"run".into(),attempt:1,kind:RunTerminalClaimKind::Abandon,claimant_pid:1111},|_|memory_core::locks::ProcessLiveness::Dead,|_|{both_inspected(&inspected);},|_,operation|{let _lock=gate.lock().unwrap();operation()}).unwrap();*finished.0.lock().unwrap()=true;finished.1.notify_all();result});
            let publish=scope.spawn(||claim_run_terminal_with_recovery(root.path(),&RunTerminalClaim{version:1,run_id:"run".into(),attempt:1,kind:RunTerminalClaimKind::Publish,claimant_pid:2222},|_|memory_core::locks::ProcessLiveness::Dead,|_|{both_inspected(&inspected);let mut done=finished.0.lock().unwrap();let (observed,timeout)=finished.1.wait_timeout_while(done,std::time::Duration::from_secs(5),|done|!*done).unwrap();done=observed;assert!(*done&&!timeout.timed_out(),"abandon claimant did not finish");},|_,operation|{let _lock=gate.lock().unwrap();operation()}).unwrap());
            [abandon.join().unwrap(),publish.join().unwrap()]
        });assert_eq!(claims[0],claims[1]);assert_eq!(claims[0].claimant_pid,1111);assert_eq!(read_run_terminal_claim(root.path()).unwrap(),claims[0]);
    }
    #[test]fn malformed_recovery_race_keeps_one_replacement(){recovery_race("");}
    #[test]fn dead_publisher_recovery_race_keeps_one_replacement(){recovery_race(r#"{"version":1,"kind":"publish","runId":"stale","attempt":1,"claimantPid":434343}"#);}
    #[test]fn live_publish_claim_keeps_ownership_against_abandonment(){
        let root=tempfile::tempdir().unwrap();let publisher=std::process::id();
        let initial=claim_run_terminal(root.path(),"run",1,RunTerminalClaimKind::Publish,publisher).unwrap();
        let result=claim_run_terminal(root.path(),"run",1,RunTerminalClaimKind::Abandon,publisher+1).unwrap();
        assert_eq!(result,initial);assert_eq!(result.kind,RunTerminalClaimKind::Publish);assert_eq!(result.claimant_pid,publisher);assert_eq!(read_run_terminal_claim(root.path()).unwrap(),initial);
    }
    #[test]fn first_claim_wins_and_no_candidates_remain(){let root=tempfile::tempdir().unwrap();let first=claim_run_terminal(root.path(),"run",1,RunTerminalClaimKind::Publish,std::process::id()).unwrap();let competing=claim_run_terminal(root.path(),"run",1,RunTerminalClaimKind::Abandon,std::process::id()).unwrap();assert_eq!(first,competing);assert!(run_terminal_claim_matches(&first,"run",1,Some(RunTerminalClaimKind::Publish)));assert!(!run_terminal_claim_matches(&first,"run",2,None));assert_eq!(std::fs::read_dir(root.path()).unwrap().count(),1);}
    #[test]fn concurrent_claims_observe_one_complete_winner(){let root=tempfile::tempdir().unwrap();let barrier=std::sync::Arc::new(std::sync::Barrier::new(8));let winners=std::thread::scope(|scope|{let handles:Vec<_>=(0..8).map(|index|{let barrier=barrier.clone();let path=root.path();scope.spawn(move||{barrier.wait();claim_run_terminal(path,"run",index,RunTerminalClaimKind::Publish,std::process::id()).unwrap()})}).collect();handles.into_iter().map(|handle|handle.join().unwrap()).collect::<Vec<_>>()});assert!(winners.iter().all(|claim|claim==&winners[0]));assert_eq!(read_run_terminal_claim(root.path()).unwrap(),winners[0]);assert_eq!(std::fs::read_dir(root.path()).unwrap().count(),1);}
    #[test]fn invalid_claim_is_not_authorized(){let root=tempfile::tempdir().unwrap();std::fs::write(run_terminal_claim_path(root.path()),r#"{"version":1,"kind":"publish","runId":"run","attempt":1,"claimantPid":0}"#).unwrap();assert!(matches!(read_run_terminal_claim(root.path()),Err(TerminalClaimError::InvalidClaim)));assert!(claim_run_terminal(root.path(),"run",1,RunTerminalClaimKind::Abandon,std::process::id()).is_err());}
}
