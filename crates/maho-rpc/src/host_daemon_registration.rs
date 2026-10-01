use crate::{host_daemon_paths::{HostDaemonPaths,generation_paths},host_daemon_state::{parse_json,read_file_or_undefined},host_reservations::read_process_start_time};
use serde_json::Value;
#[derive(Debug,PartialEq)]
pub struct RegisteredHost{pub pid:u32,pub process_start_time:Option<String>,pub socket:Option<String>,pub instance_id:String,pub generation:f64,pub writer:Option<Value>}
pub fn read_host_registration(paths:&HostDaemonPaths)->std::io::Result<Option<RegisteredHost>>{
    let pointer=read_file_or_undefined(&paths.pointer_file)?;
    let Some(pointer)=parse_json(pointer.as_deref())else{return Ok(None);};
    let Some(instance_id)=pointer.get("instance_id").and_then(Value::as_str)else{return Ok(None);};
    let text=read_file_or_undefined(&generation_paths(paths,instance_id).pid_file)?;
    let Some(record)=parse_json(text.as_deref())else{return Ok(None);};
    let Some(pid)=record.get("pid").and_then(Value::as_u64).and_then(|pid|u32::try_from(pid).ok()).filter(|pid|*pid>0)else{return Ok(None);};
    let process_start_time=match record.get("processStartTime"){
        Some(Value::Null)=>None,
        Some(Value::String(value))if !value.trim().is_empty()=>Some(value.clone()),
        _=>return Ok(None),
    };
    let writer=record.get("writer").filter(|writer|writer.is_object()&&writer["pid"].is_number()).map(|writer|serde_json::json!({"pid":writer["pid"],"startTime":writer["startTime"].as_str()}));
    Ok(Some(RegisteredHost{pid,process_start_time,socket:record.get("socket").and_then(Value::as_str).map(str::to_owned),instance_id:instance_id.into(),generation:record.get("generation").and_then(Value::as_f64).unwrap_or(0.),writer}))
}
pub fn proven_owner<'a>(registered:Option<&'a RegisteredHost>,socket:&str)->Option<&'a RegisteredHost>{
    let registered=registered?;
    if registered.socket.as_deref().is_some_and(|recorded|recorded!=socket){return None;}
    let recorded=registered.process_start_time.as_deref()?;
    (read_process_start_time(registered.pid).as_deref()==Some(recorded)).then_some(registered)
}
pub fn written_by_this_process(writer:Option<&Value>)->bool{let Some(writer)=writer else{return false;};writer["pid"].as_u64()==Some(u64::from(std::process::id()))&&writer["startTime"].as_str().is_some_and(|recorded|read_process_start_time(std::process::id()).as_deref()==Some(recorded))}
pub fn release_generation(paths:&HostDaemonPaths,instance_id:&str,pid:u32)->std::io::Result<()>{
    let generation=generation_paths(paths,instance_id);let record=read_file_or_undefined(&generation.pid_file)?;let record=parse_json(record.as_deref());
    if record.as_ref().and_then(|record|record["pid"].as_u64()).is_some_and(|recorded|recorded!=u64::from(pid)){return Ok(());}
    let pointer=read_file_or_undefined(&paths.pointer_file)?;let pointer=parse_json(pointer.as_deref());
    if pointer.as_ref().and_then(|pointer|pointer["instance_id"].as_str())==Some(instance_id){remove_file(&paths.pointer_file)?;remove_file(&paths.settings_file)?;}
    match std::fs::remove_dir_all(&generation.dir){Err(error)if error.kind()==std::io::ErrorKind::NotFound=>Ok(()),result=>result}
}
fn remove_file(path:&std::path::Path)->std::io::Result<()>{match std::fs::remove_file(path){Err(error)if error.kind()==std::io::ErrorKind::NotFound=>Ok(()),result=>result}}
#[cfg(test)]mod tests{
    use super::*;
    #[test]fn registration_requires_guard_shape_and_owner_matches_live_process(){let temp=tempfile::tempdir().unwrap();let paths=crate::host_daemon_paths::create_host_daemon_paths("socket",temp.path());crate::host_daemon_paths::create_daemon_directories(&paths).unwrap();let generation=generation_paths(&paths,"one");crate::host_daemon_paths::create_generation_directory(&generation).unwrap();std::fs::write(&paths.pointer_file,r#"{"instance_id":"one"}"#).unwrap();assert!(read_host_registration(&paths).unwrap().is_none());let pid=std::process::id();let record=serde_json::json!({"pid":pid,"processStartTime":read_process_start_time(pid),"socket":"socket","generation":3});std::fs::write(&generation.pid_file,record.to_string()).unwrap();let registered=read_host_registration(&paths).unwrap().unwrap();assert_eq!(registered.generation,3.);assert!(proven_owner(Some(&registered),"socket").is_some());assert!(proven_owner(Some(&registered),"other").is_none());let mut stale=registered;stale.process_start_time=Some("stale".into());assert!(proven_owner(Some(&stale),"socket").is_none());std::fs::write(&generation.pid_file,serde_json::json!({"pid":pid,"processStartTime":null}).to_string()).unwrap();let unguarded=read_host_registration(&paths).unwrap().unwrap();assert!(proven_owner(Some(&unguarded),"socket").is_none());}
    #[test]fn predecessor_release_preserves_successor_pointer_and_settings(){let temp=tempfile::tempdir().unwrap();let paths=crate::host_daemon_paths::create_host_daemon_paths("socket",temp.path());crate::host_daemon_paths::create_daemon_directories(&paths).unwrap();let generation=generation_paths(&paths,"old");crate::host_daemon_paths::create_generation_directory(&generation).unwrap();std::fs::write(&generation.pid_file,r#"{"pid":1}"#).unwrap();std::fs::write(&paths.pointer_file,r#"{"instance_id":"new"}"#).unwrap();std::fs::write(&paths.settings_file,"{}").unwrap();release_generation(&paths,"old",2).unwrap();assert!(generation.dir.exists());release_generation(&paths,"old",1).unwrap();assert!(!generation.dir.exists());assert!(paths.pointer_file.exists());assert!(paths.settings_file.exists());}
    #[test]fn writer_requires_current_process_start_guard(){let pid=std::process::id();assert!(!written_by_this_process(Some(&serde_json::json!({"pid":pid,"startTime":null}))));assert!(written_by_this_process(Some(&serde_json::json!({"pid":pid,"startTime":read_process_start_time(pid)}))));}
}
