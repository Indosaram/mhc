use std::{collections::HashMap,path::PathBuf};
pub const HOST_WATCH_FD_ENV:&str="SENPI_RPC_HOST_WATCH_FD";
pub const HOST_SCRATCH_DIR_ENV:&str="SENPI_RPC_HOST_SCRATCH_DIR";
pub const HOST_PUBLIC_SOCKET_ENV:&str="SENPI_RPC_HOST_PUBLIC_SOCKET";
pub const HOST_WATCH_PPID_ENV:&str="SENPI_RPC_HOST_WATCH_PPID";
pub const HOST_WATCH_PPID_INTERVAL_MS:u64=250;
pub const HOST_CLEANUP_PATHS_ENV:&str="SENPI_RPC_HOST_CLEANUP_PATHS";
#[derive(Debug,PartialEq,Eq)]
pub struct HostWatchdogConfig{pub fd:Option<u64>,pub ppid:Option<u64>,pub scratch_dir:Option<PathBuf>,pub cleanup_paths:Option<Vec<PathBuf>>,pub public_socket:Option<String>}
fn positive_integer(value:Option<&String>)->Option<u64>{let value=value?.trim();if value.is_empty()||!value.bytes().all(|byte|byte.is_ascii_digit()){return None;}value.parse::<u64>().ok().filter(|value|*value>0&&*value<=9007199254740991)}
pub fn read_host_watchdog_config(env:&HashMap<String,String>)->Option<HostWatchdogConfig>{let fd=positive_integer(env.get(HOST_WATCH_FD_ENV));let ppid=positive_integer(env.get(HOST_WATCH_PPID_ENV));if fd.is_none()&&ppid.is_none(){return None;}Some(HostWatchdogConfig{fd,ppid,scratch_dir:env.get(HOST_SCRATCH_DIR_ENV).filter(|path|!path.is_empty()).map(PathBuf::from),cleanup_paths:env.get(HOST_CLEANUP_PATHS_ENV).map(|paths|paths.split('\n').filter(|path|!path.is_empty()).map(PathBuf::from).collect()),public_socket:env.get(HOST_PUBLIC_SOCKET_ENV).filter(|path|!path.is_empty()).cloned()})}
/// The watchdog config a normal launch reads, resolving branded aliases for every canonical
/// `SENPI_RPC_HOST_*` name (senpi `readHostWatchdogConfigFromBrandEnv`).
pub fn read_host_watchdog_config_from_brand_env(env:&HashMap<String,String>)->Option<HostWatchdogConfig>{
    let mut canonical=HashMap::new();
    for (name,suffix) in [(HOST_WATCH_FD_ENV,"RPC_HOST_WATCH_FD"),(HOST_WATCH_PPID_ENV,"RPC_HOST_WATCH_PPID"),(HOST_SCRATCH_DIR_ENV,"RPC_HOST_SCRATCH_DIR"),(HOST_PUBLIC_SOCKET_ENV,"RPC_HOST_PUBLIC_SOCKET"),(HOST_CLEANUP_PATHS_ENV,"RPC_HOST_CLEANUP_PATHS")]{
        let value=env.get(name).cloned().or_else(||maho_core::brand::env_value(suffix,env));
        if let Some(value)=value{canonical.insert(name.to_owned(),value);}
    }
    read_host_watchdog_config(&canonical)
}
/**
 * Arms the configured watchdog: `on_supervisor_gone` receives the reason and is expected to
 * run the host's normal clean shutdown. `capture_before_cleanup` runs first, while the supervisor's
 * private directory still exists, so the host can read state its shutdown needs - the public-socket
 * ownership token above all (senpi `armHostWatchdog`'s `beforeCleanup`). Returns the reason future;
 * a caller awaits it, or drops it to disarm.
 */
pub async fn arm_host_watchdog(config:Option<HostWatchdogConfig>,capture_before_cleanup:impl std::future::Future<Output=Result<(),String>>,mut on_supervisor_gone:impl FnMut(String)){
    let Some(config)=config else{return;};
    let reason=if let Some(receiver)=config.fd.and_then(watch_fd_for_eof){
        match receiver.await{Ok(reason)=>Some(reason),Err(_)=>config.ppid.map(|pid|format!("supervisor pid {pid} is gone (ppid={})",parent_pid()))}
    }else if let Some(pid)=config.ppid{
        Some(watch_supervisor_parent(u32::try_from(pid).unwrap_or(u32::MAX),std::process::id(),||(parent_pid(),crate::host_reservations::process_is_live(u32::try_from(pid).unwrap_or(u32::MAX)))).await)
    }else{None};
    let Some(reason)=reason else{return;};
    let _=cleanup_watchdog_paths(&config,capture_before_cleanup).await;
    on_supervisor_gone(reason);
}
fn parent_pid()->u32{std::os::unix::process::parent_id()}
/// EOF on the inherited pipe is the primary signal; readable data is ignored, only close matters.
///
/// The blocking read lives on its OWN detached OS thread, never on the runtime's blocking pool: a
/// pooled read parked in `read(2)` is not cancelled when its task is dropped, and tokio joins every
/// blocking worker when the runtime drops, so a host that decides to exit on its own (an idle or
/// empty exit) would hang in that join until something killed it - the exact failure senpi
/// documents for a thread-pool read. A detached thread blocked on the pipe is terminated by process
/// exit and is never joined. A descriptor that cannot be reopened leaves the binding inert rather
/// than killing a healthy host.
fn watch_fd_for_eof(fd:u64)->Option<tokio::sync::oneshot::Receiver<String>>{
    use std::io::Read;
    let mut file=std::fs::File::open(format!("/dev/fd/{fd}")).ok()?;
    let(sender,receiver)=tokio::sync::oneshot::channel();
    std::thread::Builder::new().name(format!("rpc-host-watch-{fd}")).spawn(move||{
        let mut buffer=[0u8;1024];
        loop{match file.read(&mut buffer){Ok(0)|Err(_)=>{let _=sender.send(format!("supervisor pipe fd {fd} closed"));return;}Ok(_)=>{}}}
    }).ok()?;
    Some(receiver)
}
pub fn supervisor_gone_reason(supervisor_pid:u32,self_pid:u32,ppid:u32,alive:bool)->Option<String>{if supervisor_pid==self_pid{return None;}(!alive||ppid!=supervisor_pid).then(||format!("supervisor pid {supervisor_pid} is gone (ppid={ppid})"))}
pub async fn watch_supervisor_pipe(mut input:impl tokio::io::AsyncRead+Unpin)->std::io::Result<()>{
    use tokio::io::AsyncReadExt;
    let mut buffer=[0;1024];
    while input.read(&mut buffer).await?!=0{}
    Ok(())
}
pub async fn watch_supervisor_parent(supervisor_pid:u32,self_pid:u32,mut read:impl FnMut()->(u32,bool))->String{
    let period=std::time::Duration::from_millis(HOST_WATCH_PPID_INTERVAL_MS);
    let mut timer=tokio::time::interval_at(tokio::time::Instant::now()+period,period);
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop{timer.tick().await;let(ppid,alive)=read();if let Some(reason)=supervisor_gone_reason(supervisor_pid,self_pid,ppid,alive){return reason;}}
}
pub async fn cleanup_watchdog_paths(config:&HostWatchdogConfig,before_cleanup:impl std::future::Future<Output=Result<(),String>>){let _=before_cleanup.await;for path in config.cleanup_paths.iter().flatten().chain(config.scratch_dir.iter()){let _=std::fs::remove_dir_all(path).or_else(|_|std::fs::remove_file(path));}}
#[cfg(test)]mod tests{
    use super::*;
    #[test]fn requires_lifetime_binding_and_filters_empty_paths(){assert!(read_host_watchdog_config(&HashMap::new()).is_none());let env=HashMap::from([(HOST_WATCH_FD_ENV.into()," 3 ".into()),(HOST_CLEANUP_PATHS_ENV.into(),"a\n\nb".into()),(HOST_SCRATCH_DIR_ENV.into(),"".into())]);let config=read_host_watchdog_config(&env).unwrap();assert_eq!(config.fd,Some(3));assert_eq!(config.cleanup_paths.unwrap(),vec![PathBuf::from("a"),PathBuf::from("b")]);assert!(config.scratch_dir.is_none());}
    #[test]fn self_binding_is_live_and_reparenting_is_loss(){assert!(supervisor_gone_reason(1,1,0,false).is_none());assert!(supervisor_gone_reason(2,1,2,true).is_none());assert!(supervisor_gone_reason(2,1,1,true).is_some());}
    #[tokio::test]async fn capture_runs_before_cleanup_even_when_it_fails(){let temp=tempfile::tempdir().unwrap();let scratch=temp.path().join("scratch");std::fs::create_dir(&scratch).unwrap();let config=HostWatchdogConfig{fd:Some(3),ppid:None,scratch_dir:Some(scratch.clone()),cleanup_paths:None,public_socket:None};cleanup_watchdog_paths(&config,async{assert!(scratch.exists());Err("capture failed".into())}).await;assert!(!scratch.exists());}
}
