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
/// What one failed connect says about the process behind a socket entry (senpi `classifyConnectError`). Only a
/// refusal can mean "nobody": Linux answers a full backlog with EAGAIN, so ECONNREFUSED proves absence there, but
/// macOS and the BSDs refuse a live listener whose backlog is full, so a refusal there needs a second witness.
/// ENOENT means the entry is gone; every other error is a live owner (I1).
#[derive(Debug,Clone,Copy,PartialEq,Eq)]
pub enum ConnectVerdict {Live,Dead,Gone,ConfirmHolder}
pub fn classify_connect_error(code:Option<&str>,platform:&str)->ConnectVerdict{
    match code{Some("ENOENT")=>ConnectVerdict::Gone,Some("ECONNREFUSED") if platform=="linux"=>ConnectVerdict::Dead,Some("ECONNREFUSED")=>ConnectVerdict::ConfirmHolder,_=>ConnectVerdict::Live}
}
/// Whether a live process stands behind a socket entry, and the evidence for the answer.
#[derive(Debug,Clone,PartialEq,Eq)]
pub struct ListenerState {pub verdict:ListenerVerdict,pub evidence:String}
#[derive(Debug,Clone,Copy,PartialEq,Eq)]
pub enum ListenerVerdict {Live,Dead,Gone}
const HOLDER_PROBE_TIMEOUT_MS:u64=5_000;
#[cfg(unix)]
const CONNECT_PROBE_TIMEOUT_MS:u64=2_000;
/// Which processes hold the entry open (senpi `lsofSocketHolders`): pids from `lsof -t`, none, or `None` when unknown.
pub async fn lsof_socket_holders(socket_path:String)->Option<Vec<u32>>{
    let run=tokio::process::Command::new("lsof").args(["-t","--",&socket_path]).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output();
    let output=tokio::time::timeout(std::time::Duration::from_millis(HOLDER_PROBE_TIMEOUT_MS),run).await.ok()?.ok()?;
    let pids:Vec<u32>=String::from_utf8_lossy(&output.stdout).split_whitespace().filter_map(|part|part.parse().ok()).collect();
    if !pids.is_empty(){return Some(pids);}
    // lsof exits 1 with no output when nothing holds the file; anything else proves nothing.
    (output.status.success()||output.status.code()==Some(1)).then(Vec::new)
}
/// One connect to the entry: `Ok(())`, or the error code it failed with (`timeout` when it hung).
async fn connect_once(socket_path:&str)->Result<(),Option<&'static str>>{
    #[cfg(unix)]
    {
        match tokio::time::timeout(std::time::Duration::from_millis(CONNECT_PROBE_TIMEOUT_MS),tokio::net::UnixStream::connect(socket_path)).await{
            Err(_)=>Err(Some("timeout")),
            Ok(Ok(_))=>Ok(()),
            Ok(Err(error))=>Err(match error.kind(){io::ErrorKind::NotFound=>Some("ENOENT"),io::ErrorKind::ConnectionRefused=>Some("ECONNREFUSED"),_=>None}),
        }
    }
    #[cfg(not(unix))]
    { let _=socket_path; Err(None) }
}
/// senpi `socketEntryListenerState`; `holders` is [`lsof_socket_holders`] in production.
pub async fn socket_entry_listener_state<H,F>(socket_path:&str,platform:&str,holders:H)->ListenerState
where H:FnOnce(String)->F,F:std::future::Future<Output=Option<Vec<u32>>>{
    let state=|verdict,evidence:String|ListenerState{verdict,evidence};
    let code=match connect_once(socket_path).await{Ok(())=>return state(ListenerVerdict::Live,"accepting connections".into()),Err(code)=>code};
    let failed=||format!("connect {}",code.unwrap_or("failed"));
    match classify_connect_error(code,platform){
        ConnectVerdict::Live=>state(ListenerVerdict::Live,failed()),
        ConnectVerdict::Dead=>state(ListenerVerdict::Dead,failed()),
        ConnectVerdict::Gone=>state(ListenerVerdict::Gone,failed()),
        ConnectVerdict::ConfirmHolder=>match holders(socket_path.to_owned()).await{
            None=>state(ListenerVerdict::Live,"connect ECONNREFUSED, holders unknown".into()),
            Some(pids) if !pids.is_empty()=>state(ListenerVerdict::Live,format!("connect ECONNREFUSED, held open by pid {}",pids.iter().map(u32::to_string).collect::<Vec<_>>().join(","))),
            Some(_)=>state(ListenerVerdict::Dead,"connect ECONNREFUSED, held by nobody".into()),
        },
    }
}
pub fn stat_socket_identity(path:&Path)->io::Result<Option<SocketFileIdentity>>{
    #[cfg(unix)]
    {
    use std::os::unix::fs::MetadataExt;
    match fs::metadata(path){Ok(stat)=>Ok(Some(SocketFileIdentity{dev:stat.dev(),ino:stat.ino()})),Err(error) if error.kind()==io::ErrorKind::NotFound=>Ok(None),Err(error)=>Err(error)}
    }
    #[cfg(not(unix))]
    { let _=path; Ok(None) }
}
pub fn classify_endpoint_ownership(path:&str,identity:Option<SocketFileIdentity>,platform:&str)->EndpointOwnership{
    let Some(identity)=identity else{return EndpointOwnership::Unknown;};
    if platform=="win32"||path.starts_with('\0'){return EndpointOwnership::Unknown;}
    match stat_socket_identity(Path::new(path)){Ok(None)=>EndpointOwnership::Absent,Ok(Some(current)) if current==identity=>EndpointOwnership::Held,Ok(Some(_))=>EndpointOwnership::Replaced,Err(_)=>EndpointOwnership::Unknown}
}
pub fn socket_entry_replaced(path:&str,identity:Option<SocketFileIdentity>,platform:&str)->bool{classify_endpoint_ownership(path,identity,platform)==EndpointOwnership::Replaced}
/// Takes the public name over with a rename, once - and only while - that name still refers to the entry this
/// handoff was decided against (senpi `adoptPublicSocket`). A socket that changed underneath belongs to another
/// process now: the successor aborts and leaves both the intruder's socket and its own bind entry alone.
pub fn adopt_public_socket(bind_socket:&Path,public_socket:&Path,expected:Option<SocketFileIdentity>)->io::Result<()>{
    let Some(expected)=expected else{return Err(io::Error::other(format!("{}: a generation launch must name the entry it replaces",public_socket.display())));};
    if stat_socket_identity(public_socket)?!=Some(expected){return Err(io::Error::other(format!("{}: owned by another socket entry now; refusing to replace it",public_socket.display())));}
    // rename(2) is atomic for readers of the path: every connect reaches the old entry or this one, never nothing.
    fs::rename(bind_socket,public_socket)
}
pub fn write_socket_identity_file(path:&Path,identity:SocketFileIdentity)->io::Result<()>{
    if let Some(parent)=path.parent(){
        let mut builder=fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(parent)?;
    }
    let temporary=path.with_file_name(format!("{}.{}.tmp",path.file_name().unwrap_or_default().to_string_lossy(),std::process::id()));
    crate::host_daemon_state::write_state_file(&temporary,&identity).map_err(|error|error.source)?;
    fs::rename(temporary,path)
}
pub fn read_socket_identity_file(path:&Path)->io::Result<Option<SocketFileIdentity>>{
    let raw=match fs::read_to_string(path){Ok(raw)=>raw,Err(error) if error.kind()==io::ErrorKind::NotFound=>return Ok(None),Err(error)=>return Err(error)};
    Ok(serde_json::from_str(&raw).ok())
}
pub async fn wait_for_socket_identity_file(path:&Path,timeout:std::time::Duration,interval:std::time::Duration)->io::Result<Option<SocketFileIdentity>>{
    let deadline=tokio::time::Instant::now()+timeout;
    loop{
        let raw=match tokio::fs::read_to_string(path).await{
            Ok(raw)=>Some(raw),
            Err(error) if error.kind()==io::ErrorKind::NotFound=>None,
            Err(error)=>return Err(error),
        };
        if let Some(identity)=raw.and_then(|raw|serde_json::from_str(&raw).ok()){return Ok(Some(identity));}
        if tokio::time::Instant::now()>=deadline{return Ok(None);}
        tokio::time::sleep(interval).await;
    }
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
#[cfg(all(test,unix))]
mod tests{
    use super::*;
    #[test] fn adoption_refuses_a_public_name_that_changed_underneath(){let temp=tempfile::tempdir().unwrap();let public=temp.path().join("public");let bind=temp.path().join("bind");let first=std::os::unix::net::UnixListener::bind(&public).unwrap();let expected=stat_socket_identity(&public).unwrap();let ours=std::os::unix::net::UnixListener::bind(&bind).unwrap();fs::remove_file(&public).unwrap();let intruder=std::os::unix::net::UnixListener::bind(&public).unwrap();assert!(adopt_public_socket(&bind,&public,expected).is_err());assert!(bind.exists());assert!(adopt_public_socket(&bind,&public,None).is_err());let current=stat_socket_identity(&public).unwrap();adopt_public_socket(&bind,&public,current).unwrap();assert!(!bind.exists());drop((first,ours,intruder));}
    #[test] fn newer_socket_survives_teardown(){let temp=tempfile::tempdir().unwrap();let path=temp.path().join("socket");let original=std::os::unix::net::UnixListener::bind(&path).unwrap();let identity=stat_socket_identity(&path).unwrap();let replacement=temp.path().join("new");let newer=std::os::unix::net::UnixListener::bind(&replacement).unwrap();fs::rename(replacement,&path).unwrap();let path=path.to_str().unwrap();assert!(socket_entry_replaced(path,identity,"linux"));unlink_owned_socket(path,identity,"linux",|_|{});assert!(Path::new(path).exists());drop((original,newer));}
    #[test] fn own_entry_removed_and_absence_distinguished(){let temp=tempfile::tempdir().unwrap();let path=temp.path().join("socket");let listener=std::os::unix::net::UnixListener::bind(&path).unwrap();let identity=stat_socket_identity(&path).unwrap();let path=path.to_str().unwrap();assert_eq!(classify_endpoint_ownership(path,identity,"linux"),EndpointOwnership::Held);unlink_owned_socket(path,identity,"linux",|_|{});assert_eq!(classify_endpoint_ownership(path,identity,"linux"),EndpointOwnership::Absent);drop(listener);}
    #[test] fn owner_token_roundtrip_and_truncation(){let temp=tempfile::tempdir().unwrap();let path=temp.path().join("private/owner");let identity=SocketFileIdentity{dev:1,ino:2};write_socket_identity_file(&path,identity).unwrap();assert_eq!(read_socket_identity_file(&path).unwrap(),Some(identity));fs::write(&path,"{").unwrap();assert_eq!(read_socket_identity_file(&path).unwrap(),None);}
    #[tokio::test] async fn close_is_shielded_then_restored(){let temp=tempfile::tempdir().unwrap();let path=temp.path().join("socket");fs::write(&path,"entry").unwrap();shield_socket_during_close(path.to_str().unwrap(),"linux",async{assert!(!path.exists());}).await;assert_eq!(fs::read_to_string(&path).unwrap(),"entry");}
}
#[cfg(all(test,unix))]
mod listener_tests{
    use super::*;
    #[test] fn connect_errors_classify_by_platform(){
        assert_eq!(classify_connect_error(Some("ENOENT"),"darwin"),ConnectVerdict::Gone);
        assert_eq!(classify_connect_error(Some("ECONNREFUSED"),"linux"),ConnectVerdict::Dead);
        assert_eq!(classify_connect_error(Some("ECONNREFUSED"),"darwin"),ConnectVerdict::ConfirmHolder);
        assert_eq!(classify_connect_error(Some("EAGAIN"),"linux"),ConnectVerdict::Live);
        assert_eq!(classify_connect_error(None,"linux"),ConnectVerdict::Live);
    }
    #[tokio::test] async fn listener_state_reads_live_gone_and_refused_entries(){
        let temp=tempfile::tempdir().expect("tempdir");
        let path=temp.path().join("socket");
        let path_text=path.to_string_lossy().into_owned();
        let gone=socket_entry_listener_state(&path_text,"linux",|_|async{None}).await;
        assert_eq!((gone.verdict,gone.evidence.as_str()),(ListenerVerdict::Gone,"connect ENOENT"));
        let listener=std::os::unix::net::UnixListener::bind(&path).expect("bind");
        assert_eq!(socket_entry_listener_state(&path_text,"linux",|_|async{None}).await.verdict,ListenerVerdict::Live);
        drop(listener);
        assert_eq!(socket_entry_listener_state(&path_text,"linux",|_|async{None}).await.verdict,ListenerVerdict::Dead);
        let held=socket_entry_listener_state(&path_text,"darwin",|_|async{Some(vec![7,9])}).await;
        assert_eq!((held.verdict,held.evidence.as_str()),(ListenerVerdict::Live,"connect ECONNREFUSED, held open by pid 7,9"));
        let unknown=socket_entry_listener_state(&path_text,"darwin",|_|async{None}).await;
        assert_eq!(unknown.verdict,ListenerVerdict::Live);
        let nobody=socket_entry_listener_state(&path_text,"darwin",|_|async{Some(vec![])}).await;
        assert_eq!((nobody.verdict,nobody.evidence.as_str()),(ListenerVerdict::Dead,"connect ECONNREFUSED, held by nobody"));
    }
}
