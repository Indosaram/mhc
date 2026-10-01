use crate::{host_daemon_paths::{HostDaemonPaths,generation_paths},host_daemon_state::{parse_json,read_file_or_undefined},host_reservations::read_process_start_time};
use serde_json::Value;
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
    #[test]fn predecessor_release_preserves_successor_pointer_and_settings(){let temp=tempfile::tempdir().unwrap();let paths=crate::host_daemon_paths::create_host_daemon_paths("socket",temp.path());crate::host_daemon_paths::create_daemon_directories(&paths).unwrap();let generation=generation_paths(&paths,"old");crate::host_daemon_paths::create_generation_directory(&generation).unwrap();std::fs::write(&generation.pid_file,r#"{"pid":1}"#).unwrap();std::fs::write(&paths.pointer_file,r#"{"instance_id":"new"}"#).unwrap();std::fs::write(&paths.settings_file,"{}").unwrap();release_generation(&paths,"old",2).unwrap();assert!(generation.dir.exists());release_generation(&paths,"old",1).unwrap();assert!(!generation.dir.exists());assert!(paths.pointer_file.exists());assert!(paths.settings_file.exists());}
    #[test]fn writer_requires_current_process_start_guard(){let pid=std::process::id();assert!(!written_by_this_process(Some(&serde_json::json!({"pid":pid,"startTime":null}))));assert!(written_by_this_process(Some(&serde_json::json!({"pid":pid,"startTime":read_process_start_time(pid)}))));}
}
