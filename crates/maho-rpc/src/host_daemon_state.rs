use std::{fs,io,path::Path};
use serde::{Deserialize,Serialize};
use serde_json::{Map,Value};
use crate::host_daemon_paths::{HostDaemonPaths,HostDaemonStateError,HOST_STATE_FILE_MODE,create_generation_directory,generation_paths};
#[derive(Debug,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct HostDaemonSettings { pub socket:String,pub capabilities:Vec<String>,pub cold_start:String,pub idle_exit_ms:f64,pub generation:f64,pub instance_id:String }
pub fn write_host_settings(paths:&HostDaemonPaths,settings:&HostDaemonSettings) -> Result<(),HostDaemonStateError> {
    let generation=generation_paths(paths,&settings.instance_id);create_generation_directory(&generation)?;
    write_state_file(&paths.settings_file,settings)?;write_state_file(&generation.settings_file,settings)
}
pub fn read_host_settings(paths:&HostDaemonPaths) -> io::Result<Option<Map<String,Value>>> {
    let text=read_file_or_undefined(&paths.settings_file)?;let Some(parsed)=parse_json(text.as_deref()) else { return Ok(None); };
    let mut fields=Map::new();
    if let Some(value)=parsed.get("coldStart").filter(|v|v.is_string()){fields.insert("coldStart".into(),value.clone());}
    if let Some(value)=parsed.get("idleExitMs").filter(|v|v.is_number()){fields.insert("idleExitMs".into(),value.clone());}
    Ok(Some(fields))
}
pub fn write_state_file(path:&Path,content:&impl Serialize) -> Result<(),HostDaemonStateError> {
    use std::{io::Write,os::unix::fs::OpenOptionsExt};
    let result=(|| -> io::Result<()> { let mut file=fs::OpenOptions::new().write(true).create(true).truncate(true).mode(HOST_STATE_FILE_MODE).open(path)?;serde_json::to_writer(&mut file,content).map_err(io::Error::other)?;file.write_all(b"\n") })();
    result.map_err(|source|HostDaemonStateError {path:path.into(),source})
}
pub fn read_file_or_undefined(path:&Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path){Ok(text)=>Ok(Some(text)),Err(error) if matches!(error.kind(),io::ErrorKind::NotFound|io::ErrorKind::NotADirectory)=>Ok(None),Err(error)=>Err(error)}
}
pub fn parse_json(text:Option<&str>) -> Option<Map<String,Value>> { match serde_json::from_str::<Value>(text?).ok()? { Value::Object(object)=>Some(object),_=>None } }
pub fn is_record(value:&Value) -> bool {value.is_object()}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn malformed_array_and_null_are_not_records(){for text in ["[1]","null","false","bad"]{assert!(parse_json(Some(text)).is_none());}assert!(parse_json(Some("{}" )).is_some());}
    #[test] fn settings_published_in_both_locations(){let temp=tempfile::tempdir().unwrap();let paths=crate::host_daemon_paths::create_host_daemon_paths("s",temp.path());crate::host_daemon_paths::create_daemon_directories(&paths).unwrap();let settings=HostDaemonSettings{socket:"s".into(),capabilities:vec!["x".into()],cold_start:"normal".into(),idle_exit_ms:1000.,generation:0.,instance_id:"one".into()};write_host_settings(&paths,&settings).unwrap();assert_eq!(fs::read(&paths.settings_file).unwrap(),fs::read(generation_paths(&paths,"one").settings_file).unwrap());assert_eq!(read_host_settings(&paths).unwrap().unwrap().len(),2);}
    #[test] fn missing_and_not_directory_are_absent(){let temp=tempfile::tempdir().unwrap();let file=temp.path().join("file");fs::write(&file,"x").unwrap();assert!(read_file_or_undefined(&file.join("child")).unwrap().is_none());assert!(read_file_or_undefined(&temp.path().join("absent")).unwrap().is_none());}
}
