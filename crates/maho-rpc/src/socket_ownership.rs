use std::{fs,io,path::Path};
use serde::{Serialize,Deserialize};
pub const PUBLIC_SOCKET_IDENTITY_FILE:&str="public-socket.owner";
pub const SOCKET_IDENTITY_WAIT_MS:u64=30000;
pub const MAX_SOCKET_PATH_BYTES:usize=103;
#[derive(Debug,Clone,Copy,PartialEq,Eq,Serialize,Deserialize)]
pub struct SocketFileIdentity {pub dev:u64,pub ino:u64}
#[derive(Debug,Clone,Copy,PartialEq,Eq)]
pub enum EndpointOwnership {Held,Replaced,Absent,Unknown}
pub fn generation_bind_path(public_socket:&str,generation:u64)->String{format!("{public_socket}.next-{generation}")}
pub fn stat_socket_identity(path:&Path)->io::Result<Option<SocketFileIdentity>>{
    use std::os::unix::fs::MetadataExt;
    match fs::metadata(path){Ok(stat)=>Ok(Some(SocketFileIdentity{dev:stat.dev(),ino:stat.ino()})),Err(error) if error.kind()==io::ErrorKind::NotFound=>Ok(None),Err(error)=>Err(error)}
}
pub fn classify_endpoint_ownership(path:&str,identity:Option<SocketFileIdentity>,platform:&str)->EndpointOwnership{
    let Some(identity)=identity else{return EndpointOwnership::Unknown;};
    if platform=="win32"||path.starts_with('\0'){return EndpointOwnership::Unknown;}
    match stat_socket_identity(Path::new(path)){Ok(None)=>EndpointOwnership::Absent,Ok(Some(current)) if current==identity=>EndpointOwnership::Held,Ok(Some(_))=>EndpointOwnership::Replaced,Err(_)=>EndpointOwnership::Unknown}
}
pub fn socket_entry_replaced(path:&str,identity:Option<SocketFileIdentity>,platform:&str)->bool{classify_endpoint_ownership(path,identity,platform)==EndpointOwnership::Replaced}
pub fn write_socket_identity_file(path:&Path,identity:SocketFileIdentity)->io::Result<()>{
    use std::os::unix::fs::DirBuilderExt;
    if let Some(parent)=path.parent(){fs::DirBuilder::new().recursive(true).mode(0o700).create(parent)?;}
    let temporary=path.with_file_name(format!("{}.{}.tmp",path.file_name().unwrap_or_default().to_string_lossy(),std::process::id()));
    crate::host_daemon_state::write_state_file(&temporary,&identity).map_err(|error|error.source)?;
    fs::rename(temporary,path)
}
pub fn read_socket_identity_file(path:&Path)->io::Result<Option<SocketFileIdentity>>{
    let raw=match fs::read_to_string(path){Ok(raw)=>raw,Err(error) if error.kind()==io::ErrorKind::NotFound=>return Ok(None),Err(error)=>return Err(error)};
    Ok(serde_json::from_str(&raw).ok())
}
pub fn unlink_owned_socket(path:&str,identity:Option<SocketFileIdentity>,platform:&str,mut log:impl FnMut(String)){
    if platform=="win32"||path.starts_with('\0'){return;}
    let current=match stat_socket_identity(Path::new(path)){Ok(Some(current))=>current,Ok(None)=>return,Err(error)=>{log(format!("socket {path} ownership could not be verified ({error}); leaving it"));return;}};
    let Some(identity)=identity else{log(format!("socket path {path} ownership unknown; leaving it"));return;};
    if current!=identity{log(format!("socket path {path} now owned by another host; leaving it"));return;}
    if let Err(error)=fs::remove_file(path) && error.kind()!=io::ErrorKind::NotFound{log(format!("socket {path} removal failed ({error})"));}
}
pub async fn shield_socket_during_close<T>(path:&str,platform:&str,close:impl std::future::Future<Output=T>)->T{
    if platform=="win32"||path.starts_with('\0'){return close.await;}
    let shield=format!("{path}.shield-{}",std::process::id());
    let shielded=matches!(stat_socket_identity(Path::new(path)),Ok(Some(_)))&&fs::rename(path,&shield).is_ok();
    let result=close.await;
    if shielded{let _=fs::rename(shield,path);}
    result
}
#[cfg(test)]
mod tests{
    use super::*;
    #[test] fn newer_socket_survives_teardown(){let temp=tempfile::tempdir().unwrap();let path=temp.path().join("socket");let original=std::os::unix::net::UnixListener::bind(&path).unwrap();let identity=stat_socket_identity(&path).unwrap();let replacement=temp.path().join("new");let newer=std::os::unix::net::UnixListener::bind(&replacement).unwrap();fs::rename(replacement,&path).unwrap();let path=path.to_str().unwrap();assert!(socket_entry_replaced(path,identity,"linux"));unlink_owned_socket(path,identity,"linux",|_|{});assert!(Path::new(path).exists());drop((original,newer));}
    #[test] fn own_entry_removed_and_absence_distinguished(){let temp=tempfile::tempdir().unwrap();let path=temp.path().join("socket");let listener=std::os::unix::net::UnixListener::bind(&path).unwrap();let identity=stat_socket_identity(&path).unwrap();let path=path.to_str().unwrap();assert_eq!(classify_endpoint_ownership(path,identity,"linux"),EndpointOwnership::Held);unlink_owned_socket(path,identity,"linux",|_|{});assert_eq!(classify_endpoint_ownership(path,identity,"linux"),EndpointOwnership::Absent);drop(listener);}
    #[test] fn owner_token_roundtrip_and_truncation(){let temp=tempfile::tempdir().unwrap();let path=temp.path().join("private/owner");let identity=SocketFileIdentity{dev:1,ino:2};write_socket_identity_file(&path,identity).unwrap();assert_eq!(read_socket_identity_file(&path).unwrap(),Some(identity));fs::write(&path,"{").unwrap();assert_eq!(read_socket_identity_file(&path).unwrap(),None);}
    #[tokio::test] async fn close_is_shielded_then_restored(){let temp=tempfile::tempdir().unwrap();let path=temp.path().join("socket");fs::write(&path,"entry").unwrap();shield_socket_during_close(path.to_str().unwrap(),"linux",async{assert!(!path.exists());}).await;assert_eq!(fs::read_to_string(&path).unwrap(),"entry");}
}
