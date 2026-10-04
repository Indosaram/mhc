use std::{collections::HashSet,fs,path::{Path,PathBuf}};
use serde::{Serialize,Deserialize};
use sha2::{Digest,Sha256};
pub const SESSION_PATH_RETRY_AFTER_MS:u64=2000;
#[derive(Debug,Clone,Serialize,Deserialize,PartialEq)]
#[serde(rename_all="camelCase")]
pub struct SessionPathOwner{pub instance_id:String,pub pid:u32,pub process_start_time:Option<String>,pub session_path:String,#[serde(skip_serializing_if="Option::is_none")]pub attached:Option<bool>,#[serde(skip_serializing_if="Option::is_none")]pub current:Option<bool>}
pub struct SessionPathClaim{pub file:PathBuf,pub owner:SessionPathOwner}
pub fn reservation_file(dir:&Path,session_path:&str)->PathBuf{dir.join(format!("{}.json",&format!("{:x}",Sha256::digest(session_path.as_bytes()))[..16]))}
pub fn read_process_start_time(pid:u32)->Option<String>{let stat=fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;stat.rsplit_once(") ")?.1.split_whitespace().nth(19).map(str::to_owned)}
pub fn process_is_live(pid:u32)->bool{let Some(pid)=i32::try_from(pid).ok().and_then(rustix::process::Pid::from_raw)else{return false;};match rustix::process::test_kill_process(pid){Ok(())=>true,Err(error)=>error==rustix::io::Errno::PERM}}
pub fn claim_owner_is_live(owner:&SessionPathOwner)->bool{if !process_is_live(owner.pid){return false;}match (&owner.process_start_time,read_process_start_time(owner.pid)){(Some(recorded),Some(current))=>recorded==&current,_=>true}}
fn read_owner(file:&Path)->Option<SessionPathOwner>{let value:serde_json::Value=serde_json::from_str(&fs::read_to_string(file).ok()?).ok()?;Some(SessionPathOwner{instance_id:value["instanceId"].as_str()?.into(),pid:u32::try_from(value["pid"].as_u64()?).ok()?,session_path:value["sessionPath"].as_str()?.into(),process_start_time:value["processStartTime"].as_str().map(str::to_owned),attached:value["attached"].as_bool(),current:None})}
pub fn read_session_path_claims(dir:&Path)->Vec<SessionPathClaim>{let Ok(entries)=fs::read_dir(dir)else{return vec![];};entries.filter_map(Result::ok).filter(|entry|entry.file_name().to_string_lossy().ends_with(".json")).filter_map(|entry|read_owner(&entry.path()).map(|owner|SessionPathClaim{file:entry.path(),owner})).collect()}
pub fn standing_owner(mut owner:SessionPathOwner,pointer_file:&Path)->Option<SessionPathOwner>{if !claim_owner_is_live(&owner){return None;}let pointer=crate::host_daemon_state::read_file_or_undefined(pointer_file).ok().flatten();let parsed=crate::host_daemon_state::parse_json(pointer.as_deref());let current=parsed.as_ref().and_then(|record|record.get("instance_id")).and_then(serde_json::Value::as_str)==Some(owner.instance_id.as_str());if current||owner.attached!=Some(false){owner.current=Some(current);Some(owner)}else{None}}
pub struct SessionPathReservations{paths:crate::host_daemon_paths::HostDaemonPaths,instance_id:String,pid:u32,start_time:Option<String>,held:HashSet<String>}
impl SessionPathReservations{
    pub fn new(daemon_dir:&Path,instance_id:String,pid:u32)->Self{Self{paths:crate::host_daemon_paths::host_daemon_directory_paths(daemon_dir),instance_id,pid,start_time:read_process_start_time(pid),held:HashSet::new()}}
    fn publish(&self,session_path:&str,attached:bool)->std::io::Result<()>{use std::os::unix::fs::DirBuilderExt;let owner=SessionPathOwner{instance_id:self.instance_id.clone(),pid:self.pid,process_start_time:self.start_time.clone(),session_path:session_path.into(),attached:Some(attached),current:None};fs::DirBuilder::new().recursive(true).mode(0o700).create(&self.paths.reservations_dir)?;let file=reservation_file(&self.paths.reservations_dir,session_path);let staging=file.with_file_name(format!("{}.{}.tmp",file.file_name().unwrap_or_default().to_string_lossy(),self.pid));crate::host_daemon_state::write_state_file(&staging,&owner).map_err(|error|error.source)?;fs::rename(staging,file)}
    pub fn claim(&mut self,session_path:&str,attached:bool,mut report:impl FnMut(String))->Option<SessionPathOwner>{if let Some(existing)=read_owner(&reservation_file(&self.paths.reservations_dir,session_path))&&existing.pid!=self.pid&&let Some(standing)=standing_owner(existing,&self.paths.pointer_file){return Some(standing);}match self.publish(session_path,attached){Ok(())=>{self.held.insert(session_path.into());},Err(error)=>report(format!("session path reservation for {session_path} could not be written ({error})"))}None}
    pub fn release(&mut self,session_path:&str,mut report:impl FnMut(String)){
        if !self.held.remove(session_path){return;}
        if let Err(error)=fs::remove_file(reservation_file(&self.paths.reservations_dir,session_path))&&error.kind()!=std::io::ErrorKind::NotFound{report(format!("session path reservation for {session_path} could not be removed ({error})"));}
    }
    pub fn set_attached(&self,session_path:&str,attached:bool,mut report:impl FnMut(String)){if self.held.contains(session_path)&&let Err(error)=self.publish(session_path,attached){report(format!("session path reservation for {session_path} could not be updated ({error})"));}}
}
/// A path-reservation set with its failure reporter bound once (senpi's factory shape).
pub struct BoundPathReservations{inner:SessionPathReservations,report:Box<dyn FnMut(String)+Send>}
impl BoundPathReservations{
    pub fn claim(&mut self,session_path:&str,attached:bool)->Option<SessionPathOwner>{let report=&mut self.report;self.inner.claim(session_path,attached,report)}
    pub fn release(&mut self,session_path:&str){let report=&mut self.report;self.inner.release(session_path,report);}
    pub fn set_attached(&mut self,session_path:&str,attached:bool){let report=&mut self.report;self.inner.set_attached(session_path,attached,report);}
}
/// This endpoint's daemon directory: the claims live in it, and the pointer they are read
/// against lives there too (senpi `createSessionPathReservations`).
pub fn create_session_path_reservations(daemon_dir:&Path,instance_id:String,pid:Option<u32>,on_failure:impl FnMut(String)+Send+'static)->BoundPathReservations{
    BoundPathReservations{inner:SessionPathReservations::new(daemon_dir,instance_id,pid.unwrap_or_else(std::process::id)),report:Box::new(on_failure)}
}
/// The claims a shared host publishes, or nothing when it serves no socket and no daemon
/// directory was told through the environment (senpi `createEndpointReservations`).
pub fn create_endpoint_reservations(agent_dir:&Path,socket:Option<&str>,instance_id:String,on_failure:impl FnMut(String)+Send+'static)->Option<BoundPathReservations>{
    let told=std::env::var(crate::host_daemon_paths::HOST_DAEMON_DIR_ENV).ok().filter(|value|!value.trim().is_empty());
    let dir=match told{Some(dir)=>PathBuf::from(dir),None=>match socket{Some(socket)=>crate::host_daemon_paths::create_host_daemon_paths(socket,agent_dir).dir,None=>return None}};
    Some(create_session_path_reservations(&dir,instance_id,None,on_failure))
}
#[cfg(test)]
mod tests{
    use super::*;
    #[test]fn bound_reservations_claim_republish_and_release_through_the_reporter(){let temp=tempfile::tempdir().unwrap();let failures=std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));let recorder=failures.clone();let mut reservations=create_session_path_reservations(temp.path(),"one".into(),None,move|message|recorder.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(message));assert!(reservations.claim("/session",true).is_none());let dir=temp.path().join("reservations");assert_eq!(read_session_path_claims(&dir).len(),1);reservations.set_attached("/session",false);assert_eq!(read_session_path_claims(&dir)[0].owner.attached,Some(false));reservations.release("/session");assert!(read_session_path_claims(&dir).is_empty());assert!(failures.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_empty());}
    #[test]fn endpoint_reservations_need_a_socket_or_a_daemon_directory(){let temp=tempfile::tempdir().unwrap();assert!(create_endpoint_reservations(temp.path(),None,"one".into(),|_|{}).is_none());let bound=create_endpoint_reservations(temp.path(),Some("/tmp/rpc.sock"),"one".into(),|_|{});assert!(bound.is_some());}
    #[test]fn claims_republish_and_release(){let temp=tempfile::tempdir().unwrap();let mut reservations=SessionPathReservations::new(temp.path(),"one".into(),std::process::id());assert!(reservations.claim("/session",true,|error|panic!("{error}")).is_none());let dir=temp.path().join("reservations");assert_eq!(read_session_path_claims(&dir).len(),1);reservations.set_attached("/session",false,|error|panic!("{error}"));assert_eq!(read_session_path_claims(&dir)[0].owner.attached,Some(false));reservations.release("/session",|error|panic!("{error}"));assert!(read_session_path_claims(&dir).is_empty());}
    #[test]fn superseded_detached_claim_is_reclaimable_but_current_stands(){let temp=tempfile::tempdir().unwrap();let pointer=temp.path().join("host.pid");let owner=SessionPathOwner{instance_id:"one".into(),pid:std::process::id(),process_start_time:read_process_start_time(std::process::id()),session_path:"/session".into(),attached:Some(false),current:None};assert!(standing_owner(owner.clone(),&pointer).is_none());fs::write(&pointer,r#"{"instance_id":"one"}"#).unwrap();assert_eq!(standing_owner(owner.clone(),&pointer).unwrap().current,Some(true));let mut recycled=owner;recycled.process_start_time=Some("not-current".into());assert!(!claim_owner_is_live(&recycled));}
}
