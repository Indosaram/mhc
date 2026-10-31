use crate::{host_protocol_info::HostProtocolInfo,socket_ownership::{SocketFileIdentity,stat_socket_identity}};
pub const DEFAULT_HANDOFF_READINESS_MS:u64=30_000;
pub struct SuccessorLaunchOptions{pub launch:crate::host_launch::HostLaunch,pub env:std::collections::HashMap<String,String>,pub instance_id:String,pub generation:u64,pub launch_profile_id:String,pub readiness_ms:u64}
pub struct RunningSuccessor{pub child:tokio::process::Child,pub protocol:HostProtocolInfo}
/// Asked once the successor owns the socket: the drain request is sent only when it answers true (senpi `drainGate`).
pub type DrainGate=std::pin::Pin<Box<dyn std::future::Future<Output=bool>+Send>>;
/// How long a refused handoff waits for the successor it killed to exit before releasing its record.
const SUCCESSOR_EXIT_WAIT_MS:u64=5_000;
/// The longest `start_successor` runs: its start-time read, the readiness window, one last probe and a refusal's exit wait.
pub const SUCCESSOR_START_BUDGET_MS:u64=10_000+DEFAULT_HANDOFF_READINESS_MS+2_000+SUCCESSOR_EXIT_WAIT_MS;
/// senpi `startSuccessor`. `context.owner` is the predecessor's proven identity; `None` when nothing proves it, and
/// then it is never signalled. `boot_settings` is the endpoint `settings.json` as it was before `prepare_successor`
/// overwrote it: a refused handoff puts it back byte for byte.
pub async fn start_successor(context:&crate::host_handoff::HandoffContext,socket:&str,replaced:SocketFileIdentity,options:SuccessorLaunchOptions)->std::io::Result<Result<RunningSuccessor,crate::host_handoff::HandoffRefusal>>{
    start_successor_gated(context,socket,replaced,options,None,None).await
}
pub async fn start_successor_gated(context:&crate::host_handoff::HandoffContext,socket:&str,replaced:SocketFileIdentity,options:SuccessorLaunchOptions,boot_settings:Option<Option<String>>,drain_gate:Option<DrainGate>)->std::io::Result<Result<RunningSuccessor,crate::host_handoff::HandoffRefusal>>{
    let mut stderr_options=std::fs::OpenOptions::new();stderr_options.create(true).append(true);
    #[cfg(unix)] {use std::os::unix::fs::OpenOptionsExt;stderr_options.mode(0o600);}
    let stderr=stderr_options.open(&context.paths.stderr_log)?;
    // senpi `startSuccessor`: an enforcing predecessor never downgrades, and the presenter channel enrolls this
    // replacement against the host it replaces.
    let bootstrap=crate::mcp_presenter_bootstrap::take_presenter_bootstrap();
    let enforce=crate::mcp_presenter_bootstrap::presenter_enforcement()||context.host.capabilities.iter().any(|capability|capability==crate::host_capabilities::MCP_FORM_PRESENTER_CAPABILITY);
    let args:Vec<String>=options.launch.args.iter().cloned().chain(crate::mcp_presenter_bootstrap::presenter_launch_args(bootstrap.is_some(),enforce)).collect();
    let env=options.env;
    let configure=|command:&mut tokio::process::Command|{command.env_clear().envs(env).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(stderr);#[cfg(unix)] command.process_group(0);};
    #[cfg(unix)]
    let spawned=crate::mcp_presenter_bootstrap::spawn_with_presenter_bootstrap(options.launch.command.as_os_str(),&args,configure,bootstrap,crate::mcp_presenter_bootstrap::PresenterLaunch{kind:"replacement",predecessor_host_instance:context.host.instance_id.clone().map(Some)});
    #[cfg(not(unix))]
    let spawned={drop(bootstrap);let mut command=tokio::process::Command::new(&options.launch.command);command.args(&args);configure(&mut command);command.spawn().map(|child|(child,None::<tokio::task::JoinHandle<()>>))};
    let(mut child,presenter_enrollment)=match spawned{
        Ok(spawned)=>spawned,
        Err(error)=>{restore_boot_settings(&context.paths,boot_settings.as_ref());let _=std::fs::remove_dir_all(crate::host_daemon_paths::generation_paths(&context.paths,&options.instance_id).dir);return Err(error);}
    };
    let pid=child.id().ok_or_else(||std::io::Error::other("successor has no process identity"))?;
    let launch_profile_id=options.launch_profile_id;
    let result:std::io::Result<Option<HostProtocolInfo>>=async{
        let process_start_time=wait_for_start_time(pid,SUCCESSOR_START_TIME_WAIT_MS,read_start_time_off_runtime,crate::host_reservations::process_is_live).await.ok().flatten();
        // No build yet: until the successor answers, its record claims no engine version rather than this process's.
        let mut registration=crate::host_daemon_registration::HostRegistration{pid,process_start_time,socket:socket.into(),instance_id:options.instance_id.clone(),generation:options.generation as f64,launch_profile_id,build:None};
        // The successor is running from this instant: its own record says so, so `host gc` never judges the
        // endpoint by a predecessor that died meanwhile. The pointer moves only once the rename landed.
        crate::host_daemon_registration::write_generation_record(&context.paths,&registration).map_err(|error|error.source)?;
        let deadline=tokio::time::Instant::now()+std::time::Duration::from_millis(options.readiness_ms);
        loop{
            if let Some(answer)=crate::host_probe::probe_protocol_info(socket,2000).await&&successor_answered(&context.host,&answer){
                // The successor's build as the successor itself reported it on the socket; never this process's.
                registration.build=crate::host_daemon_registration::RegisteredBuild::answered(&answer);
                crate::host_daemon_registration::write_host_registration(&context.paths,&registration).map_err(|error|error.source)?;
                return Ok(Some(answer));
            }
            if child.try_wait()?.is_some()||tokio::time::Instant::now()>=deadline{return Ok(None);}
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }.await;
    match result{
        Ok(Some(protocol))=>{
            if let Some(enrollment)=presenter_enrollment{let _=enrollment.await;}
            // The successor owns the socket now: the predecessor may drain. SIGUSR1 goes only to a pid the record
            // proved and a host that advertised it can survive the signal.
            let drain=match drain_gate{Some(gate)=>gate.await,None=>true};
            #[cfg(unix)]
            {if let Some(owner)=&context.owner&&drain{crate::host_stop::signal_generation(owner.pid,rustix::process::Signal::USR1).map_err(std::io::Error::from)?;}}
            #[cfg(not(unix))]
            let _=drain;
            Ok(Ok(RunningSuccessor{child,protocol}))
        }
        result=>{
            if let Some(enrollment)=presenter_enrollment{enrollment.abort();}
            let reason=if result.is_err(){"successor_unavailable"}else{abort_reason(std::path::Path::new(socket),replaced)};
            let detail=abandon_successor(&mut child,&context.paths,&options.instance_id,boot_settings.as_ref(),if result.is_err(){"successor_start_failed"}else{"successor_readiness_timeout"}).await;
            let detail=match(result.err(),detail){(Some(cause),Some(cleanup))=>Some(format!("{cause}; {cleanup}")),(Some(cause),None)=>Some(cause.to_string()),(None,cleanup)=>cleanup};
            Ok(Err(crate::host_handoff::HandoffRefusal{reason,upgradeable:true,detail}))
        }
    }
}
/// How long a spawned successor's start time is waited for before it is recorded as unknown.
const SUCCESSOR_START_TIME_WAIT_MS:u64=10_000;
/// senpi `waitForStartTime`: the process start time once it is readable; `None` when the process is live but its
/// start time never became readable in `timeout_ms`; an error when the process is gone without one.
pub async fn wait_for_start_time<F:std::future::Future<Output=Option<String>>>(pid:u32,timeout_ms:u64,read_start_time:impl Fn(u32)->F,is_live:impl Fn(u32)->bool)->std::io::Result<Option<String>>{
    let deadline=tokio::time::Instant::now()+std::time::Duration::from_millis(timeout_ms);
    loop{
        if let Some(start)=read_start_time(pid).await.filter(|start|!start.is_empty()){return Ok(Some(start));}
        if tokio::time::Instant::now()>=deadline{break;}
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    if is_live(pid){return Ok(None);}
    Err(std::io::Error::other(format!("spawned daemon pid {pid} had no process start time")))
}
/// The start-time read runs on the blocking pool: on Windows it spawns powershell synchronously.
async fn read_start_time_off_runtime(pid:u32)->Option<String>{
    tokio::task::spawn_blocking(move||crate::host_reservations::read_process_start_time(pid)).await.ok().flatten()
}
fn restore_boot_settings(paths:&crate::host_daemon_paths::HostDaemonPaths,boot_settings:Option<&Option<String>>)->Option<String>{
    let result=match boot_settings{
        None=>Ok(()),
        Some(None)=>match std::fs::remove_file(&paths.settings_file){Err(error) if error.kind()!=std::io::ErrorKind::NotFound=>Err(error),_=>Ok(())},
        Some(Some(text))=>std::fs::write(&paths.settings_file,text),
    };
    result.err().map(|cause|format!("successor cleanup failed: {cause}"))
}
/// Undoes what a refused handoff left (senpi `abandonSuccessor`): kills the successor - recording the stop it could
/// not be asked for, since a SIGKILLed process runs no exit handler - releases its generation once it has exited,
/// and puts the boot settings back. A cleanup that fails is answered, not thrown.
async fn abandon_successor(child:&mut tokio::process::Child,paths:&crate::host_daemon_paths::HostDaemonPaths,instance_id:&str,boot_settings:Option<&Option<String>>,reason:&str)->Option<String>{
    let target=crate::host_ensure_stop::StopTarget{daemon_dir:paths.dir.clone(),generation:crate::host_daemon_paths::generation_paths(paths,instance_id),instance_id:instance_id.into(),sender:crate::host_stop_intent::HostStopSender{pid:std::process::id(),kind:"successor".into(),generation:None},reason:reason.into()};
    let pid=child.id();
    let killed=match(pid,child.try_wait()){(Some(pid),Ok(None))=>Some(crate::host_ensure_stop::announce_stop(&target,pid)),_=>None};
    if killed.is_some(){let _=child.start_kill();}
    let restored=restore_boot_settings(paths,boot_settings);
    let exited=tokio::time::timeout(std::time::Duration::from_millis(SUCCESSOR_EXIT_WAIT_MS),child.wait()).await.is_ok();
    if exited&&let Some(pid)=pid{
        if let Some(killed)=&killed{crate::host_ensure_stop::record_escalation(&target,killed);}
        if let Err(cause)=crate::host_daemon_registration::release_generation(paths,instance_id,pid){return Some(format!("successor cleanup failed: {cause}"));}
    }
    restored
}
#[derive(Debug,PartialEq,Eq)]pub struct SuccessorPreparation{pub bind_socket:String,pub replaced:SocketFileIdentity,pub argv:Vec<String>}
#[derive(Debug,thiserror::Error)]pub enum PrepareSuccessorError{
    #[error("socket_path_too_long: {0}")]SocketPathTooLong(String),
    #[error("socket_replaced")]SocketReplaced,
    #[error(transparent)]Io(#[from]std::io::Error),
    #[error(transparent)]State(#[from]crate::host_daemon_paths::HostDaemonStateError),
}
pub fn prepare_successor(socket:&str,paths:&crate::host_daemon_paths::HostDaemonPaths,generation:u64,instance_id:&str,policy:Option<&crate::host_lifecycle::HostLifecyclePolicy>,host_args:&[String])->Result<SuccessorPreparation,PrepareSuccessorError>{
    let bind_socket=crate::socket_ownership::generation_bind_path(socket,generation);
    if bind_socket.len()>crate::socket_ownership::MAX_SOCKET_PATH_BYTES{return Err(PrepareSuccessorError::SocketPathTooLong(bind_socket));}
    let replaced=stat_socket_identity(std::path::Path::new(socket))?.ok_or(PrepareSuccessorError::SocketReplaced)?;
    let running=crate::host_daemon_state::read_host_settings(paths)?;
    let cold_start=policy.map(|policy|policy.cold_start.clone()).or_else(||running.as_ref().and_then(|running|running.get("coldStart")).and_then(serde_json::Value::as_str).map(str::to_owned)).unwrap_or_else(||"transient".into());
    let idle_exit_ms=policy.map(|policy|policy.idle_exit_ms).or_else(||running.as_ref().and_then(|running|running.get("idleExitMs")).and_then(serde_json::Value::as_f64)).unwrap_or(crate::host_lifecycle::DEFAULT_HOST_IDLE_EXIT_MS);
    crate::host_daemon_state::write_host_settings(paths,&crate::host_daemon_state::HostDaemonSettings{socket:socket.into(),capabilities:crate::host_launch::PINNED_HOST_CLIENT_CAPABILITIES.iter().map(|value|(*value).into()).collect(),cold_start,idle_exit_ms,generation:generation as f64,instance_id:instance_id.into()})?;
    let argv=["--socket".into(),socket.into(),"--bind".into(),bind_socket.clone(),"--replace".into(),format!("{}:{}",replaced.dev,replaced.ino)].into_iter().chain(host_args.iter().cloned()).collect();
    Ok(SuccessorPreparation{bind_socket,replaced,argv})
}
pub fn successor_env(mut env:std::collections::HashMap<String,String>,generation:u64,instance_id:&str,daemon_dir:&str,agent_dir:Option<&str>,overrides:&std::collections::HashMap<String,Option<String>>)->std::collections::HashMap<String,String>{
    env.insert(crate::protocol_identity::HOST_GENERATION_ENV.into(),generation.to_string());
    env.insert(crate::protocol_identity::HOST_INSTANCE_ID_ENV.into(),instance_id.into());
    env.insert(crate::host_daemon_paths::HOST_DAEMON_DIR_ENV.into(),daemon_dir.into());
    env.insert(crate::custom_capability::RPC_CLIENT_CAPABILITIES_ENV.into(),crate::host_launch::PINNED_HOST_CLIENT_CAPABILITIES.join(","));
    if let Some(agent_dir)=agent_dir.filter(|dir|!dir.is_empty()){env.insert(maho_core::config::env_agent_dir_var(),agent_dir.into());}
    for(key,value)in overrides{if let Some(value)=value{env.insert(key.clone(),value.clone());}else{env.remove(key);}}
    env
}
#[cfg(test)]mod env_tests{use super::*;use std::collections::HashMap;#[test]fn successor_identity_replaces_parent_and_nullable_overrides_apply_last(){let parent=HashMap::from([(crate::protocol_identity::HOST_INSTANCE_ID_ENV.into(),"old".into()),("DROP".into(),"old".into())]);let env=successor_env(parent,2,"new","/daemon",None,&HashMap::from([("DROP".into(),None),("X".into(),Some("y".into()))]));assert_eq!(env[crate::protocol_identity::HOST_INSTANCE_ID_ENV],"new");assert_eq!(env[crate::protocol_identity::HOST_GENERATION_ENV],"2");assert_eq!(env[crate::custom_capability::RPC_CLIENT_CAPABILITIES_ENV],"extension_events,custom_unsupported");assert!(!env.contains_key("DROP"));assert_eq!(env["X"],"y");}}
pub fn successor_answered(previous:&HostProtocolInfo,answer:&HostProtocolInfo)->bool{answer.instance_id.as_ref().is_some_and(|id|Some(id)!=previous.instance_id.as_ref())}
pub fn abort_reason(socket:&std::path::Path,replaced:SocketFileIdentity)->&'static str{if stat_socket_identity(socket).ok().flatten()==Some(replaced){"successor_unavailable"}else{"socket_replaced"}}
#[cfg(test)]mod start_time_tests{
    use super::*;
    #[tokio::test(start_paused=true)]
    async fn start_time_is_waited_for_and_a_gone_process_without_one_is_an_error(){
        let reads=std::cell::Cell::new(0);
        let read=|_pid:u32|{reads.set(reads.get()+1);(reads.get()>=3).then(||"t".to_owned())};
        assert_eq!(wait_for_start_time(1,10_000,|pid|std::future::ready(read(pid)),|_|true).await.unwrap().as_deref(),Some("t"));
        assert_eq!(reads.get(),3);
        assert_eq!(wait_for_start_time(1,100,|_|std::future::ready(None),|_|true).await.unwrap(),None);
        let error=wait_for_start_time(7,100,|_|std::future::ready(None),|_|false).await.unwrap_err();
        assert_eq!(error.to_string(),"spawned daemon pid 7 had no process start time");
    }
}
#[cfg(test)]mod tests{use super::*;#[test]fn public_answer_must_name_different_instance(){let previous=crate::host_protocol_info::parse_host_protocol_info(&serde_json::json!({"protocolVersion":1,"serverVersion":"1","capabilities":[],"instanceId":"old"})).unwrap();let mut answer=previous.clone();assert!(!successor_answered(&previous,&answer));answer.instance_id=None;assert!(!successor_answered(&previous,&answer));answer.instance_id=Some("new".into());assert!(successor_answered(&previous,&answer));}#[cfg(unix)] #[test]fn abort_reason_observes_real_socket_entry(){let temp=tempfile::tempdir().unwrap();let path=temp.path().join("s");let socket=std::os::unix::net::UnixListener::bind(&path).unwrap();let identity=stat_socket_identity(&path).unwrap().unwrap();assert_eq!(abort_reason(&path,identity),"successor_unavailable");drop(socket);std::fs::remove_file(&path).unwrap();assert_eq!(abort_reason(&path,identity),"socket_replaced");}}
