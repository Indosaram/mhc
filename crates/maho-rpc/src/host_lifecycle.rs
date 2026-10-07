use std::collections::HashMap;
pub use crate::host_child_exit::classify_child_exit;
pub const HOST_COLD_START_ENV:&str="SENPI_RPC_HOST_COLD_START";
pub const HOST_IDLE_EXIT_MS_ENV:&str="SENPI_RPC_HOST_IDLE_EXIT_MS";
pub const DEFAULT_HOST_IDLE_EXIT_MS:f64=900000.;
pub const HANDOFF_GRACE_MS_ENV:&str="SENPI_RPC_HANDOFF_GRACE_MS";
pub const DEFAULT_HANDOFF_GRACE_MS:f64=600000.;
pub use crate::host_launch_spec::HostLifecyclePolicyInput;
pub const INTERNAL_SUPERVISOR_FLAG:&str="--internal-rpc-host-supervisor";
/// The stdio slot senpi gives the child's lifetime pipe (senpi `CHILD_WATCH_FD`). Rust's `Command`
/// exposes only the three standard slots, so the pipe is inherited by descriptor instead - see
/// `bind_child_watch_pipe`.
pub const CHILD_WATCH_FD:u32=3;
/// Gives `child_env` the child's lifetime binding and returns the pipe ends this process keeps
/// alive for the whole supervisor run (senpi host-lifecycle.ts:513-544, stdio slot 3).
///
/// senpi duplicates the read end into stdio slot 3. Rust's `Command` exposes only the three
/// standard slots and `pre_exec` is unavailable under this crate's `unsafe_code = "forbid"`, so the
/// read end's close-on-exec flag is cleared instead: the child inherits the pipe at the number this
/// process holds it at, and `HOST_WATCH_FD_ENV` carries exactly that number. The write end stays
/// here, close-on-exec, is never inherited and is never written - only its close matters.
#[cfg(unix)]
fn bind_child_watch_pipe(child_env:&mut HashMap<String,String>)->std::io::Result<(std::os::fd::OwnedFd,std::os::fd::OwnedFd)>{
    use std::os::fd::AsRawFd;
    let (read_end,write_end)=rustix::pipe::pipe()?;
    rustix::io::fcntl_setfd(&read_end,rustix::io::FdFlags::empty()).map_err(std::io::Error::from)?;
    let descriptor=u64::try_from(read_end.as_raw_fd()).map_err(|_|std::io::Error::other("child watch descriptor is not representable"))?;
    child_env.insert(crate::host_watchdog::HOST_WATCH_FD_ENV.into(),descriptor.to_string());
    Ok((read_end,write_end))
}
#[derive(Clone,Copy,Default)]
pub struct HostActivity{pub connections:u64,pub active_turns:u64}
#[derive(Default)]pub struct ObservedHostTurns{busy:HashMap<String,u64>}
impl ObservedHostTurns{
    pub async fn read_events(&mut self,mut input:impl tokio::io::AsyncRead+Unpin,mut changed:impl FnMut(&Self))->std::io::Result<()>{
        use tokio::io::AsyncReadExt;
        let mut lines=crate::jsonl::JsonlLineReader::new(crate::jsonl::MAX_RPC_LINE_CHARACTERS).expect("positive line limit");
        let mut buffer=[0;8192];
        loop{
            let size=input.read(&mut buffer).await?;
            let records=if size==0{lines.finish()}else{lines.push(&buffer[..size])};
            for record in records{if let crate::jsonl::LineRecord::Line(line)=record&&self.observe(&line){changed(self);}}
            if size==0{return Ok(());}
        }
    }
    pub fn busy_turns(&self)->u64{self.busy.values().filter(|count|**count>0).count() as u64}
    pub fn observe(&mut self,line:&str)->bool{
        let Ok(event)=serde_json::from_str::<serde_json::Value>(line)else{return false;};
        let Some(session)=event["sessionId"].as_str()else{return false;};
        match event["type"].as_str(){
            Some("agent_start")=>{*self.busy.entry(session.into()).or_default()+=1;true},
            Some("agent_settled")=>{let count=self.busy.entry(session.into()).or_insert(1);*count=count.saturating_sub(1);true},
            _=>false,
        }
    }
    pub fn activity(&self,connections:u64,observer:&crate::observer_link::ObserverLink,now:f64,unknown_grace_ms:f64)->HostActivity{
        HostActivity{connections,active_turns:crate::observer_link::active_turns_for_idle_decision(&crate::observer_link::UnknownActivityInput{healthy:observer.healthy(),unhealthy_since:observer.unhealthy_since(),now,unknown_grace_ms,observed_busy:self.busy.values().filter(|count|**count>0).count() as u64})}
    }
}
pub async fn wait_for_idle_exit(idle_exit_ms:f64,mut activity:tokio::sync::watch::Receiver<HostActivity>)->Result<(),tokio::sync::watch::error::RecvError>{
    let mut decider=IdleExitDecider::new(idle_exit_ms);
    let started=tokio::time::Instant::now();
    let period=std::time::Duration::from_secs_f64((idle_exit_ms/4.).clamp(20.,1000.)/1000.);
    let mut ticker=tokio::time::interval(period);ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop{
        let state=*activity.borrow_and_update();
        if decider.update(state.connections,state.active_turns,started.elapsed().as_secs_f64()*1000.)==IdleExitDecision::Exit{return Ok(());}
        tokio::select!{
            ()=async{ticker.tick().await;}=>{},
            changed=activity.changed()=>changed?,
        }
    }
}
pub async fn proxy_connection(mut client:tokio::net::UnixStream,internal_socket:&std::path::Path,internal_secret:Option<&[u8]>)->std::io::Result<()> {
    let mut internal=tokio::net::UnixStream::connect(internal_socket).await?;
    if let Some(secret)=internal_secret{crate::socket_transport::send_socket_handshake(&mut internal,secret).await?;}
    tokio::io::copy_bidirectional(&mut client,&mut internal).await?;
    Ok(())
}
pub async fn run_socket_proxy(listener:tokio::net::UnixListener,internal_socket:std::path::PathBuf,public_secret:Option<Vec<u8>>,internal_secret:Option<Vec<u8>>,mut draining:tokio::sync::watch::Receiver<bool>,connections:tokio::sync::watch::Sender<usize>)->std::io::Result<()> {
    let mut clients=tokio::task::JoinSet::new();
    loop{
        if *draining.borrow(){break;}
        tokio::select!{
            biased;
            changed=draining.changed()=>{if changed.is_err()||*draining.borrow(){break;}},
            result=clients.join_next(),if !clients.is_empty()=>{let _=result;connections.send_replace(clients.len());},
            accepted=listener.accept()=>{
                let(mut client,_)=accepted?;
                if *draining.borrow(){break;}
                let path=internal_socket.clone();let public=public_secret.clone();let internal=internal_secret.clone();
                clients.spawn(async move{
                    if let Some(secret)=public{crate::socket_transport::authenticate_socket(&mut client,&secret).await?;}
                    proxy_connection(client,&path,internal.as_deref()).await
                });
                connections.send_replace(clients.len());
            }
        }
    }
    drop(listener);
    while clients.join_next().await.is_some(){connections.send_replace(clients.len());}
    Ok(())
}
#[derive(Debug,PartialEq,Eq)]
pub struct SupervisorLaunch{pub socket:String,pub host_args:Vec<String>,pub child_command:Option<String>,pub child_args:Option<Vec<String>>,pub agent_dir:Option<String>,pub bind_socket:Option<String>,pub replace_identity:Option<crate::socket_ownership::SocketFileIdentity>}
pub fn find_internal_supervisor_args(argv:&[String])->Option<&[String]>{
    let mut index=0;
    while index<argv.len(){
        if argv[index]==INTERNAL_SUPERVISOR_FLAG{return Some(&argv[index+1..]);}
        if argv[index]!="--extension"||index+1>=argv.len(){return None;}
        index+=2;
    }
    None
}
pub fn parse_supervisor_args(argv:&[String])->Option<SupervisorLaunch>{
    let mut launch=SupervisorLaunch{socket:String::new(),host_args:vec![],child_command:None,child_args:None,agent_dir:None,bind_socket:None,replace_identity:None};
    let mut socket_seen=false;let mut index=0;
    while index<argv.len(){
        let arg=&argv[index];
        if index+1<argv.len()&&matches!(arg.as_str(),"--socket"|"--child-command"|"--child-args"|"--agent-dir"|"--bind"|"--replace"){
            index+=1;let value=&argv[index];
            match arg.as_str(){
                "--socket"=>{launch.socket=value.clone();socket_seen=true;},
                "--child-command"=>launch.child_command=Some(value.clone()),
                "--child-args"=>{let parsed:serde_json::Value=serde_json::from_str(value).ok()?;launch.child_args=parsed.as_array().and_then(|items|items.iter().map(|item|item.as_str().map(str::to_owned)).collect());},
                "--agent-dir"=>launch.agent_dir=Some(value.clone()),
                "--bind"=>launch.bind_socket=Some(value.clone()),
                "--replace"=>launch.replace_identity=value.split_once(':').filter(|(dev,ino)|!dev.is_empty()&&!ino.is_empty()&&dev.bytes().chain(ino.bytes()).all(|byte|byte.is_ascii_digit())).and_then(|(dev,ino)|Some(crate::socket_ownership::SocketFileIdentity{dev:dev.parse().ok()?,ino:ino.parse().ok()?})),
                _=>unreachable!(),
            }
        }else{launch.host_args.push(arg.clone());}
        index+=1;
    }
    socket_seen.then_some(launch)
}
/**
 * The host supervisor: bind the public endpoint, run a private host behind it, proxy clients,
 * and leave when idle or when the endpoint is taken over (senpi `runHostSupervisor`).
 *
 * The supervisor owns the public secret on win32 and the internal hop's directory; the child
 * only sees the internal socket. It re-enters the committed CLI entry so the host it serves is
 * built from this same executable.
 */
pub async fn run_host_supervisor(launch:SupervisorLaunch)->std::io::Result<()>{
    let platform=if cfg!(windows){"win32"}else{std::env::consts::OS};
    let agent_dir=launch.agent_dir.clone().unwrap_or_else(maho_core::config::get_agent_dir);
    let paths=crate::host_daemon_paths::create_host_daemon_paths(&launch.socket,std::path::Path::new(&agent_dir));
    crate::host_daemon_paths::create_daemon_directories(&paths).map_err(|error|error.source)?;
    let env:HashMap<String,String>=std::env::vars().collect();
    let instance_id=env.get(crate::protocol_identity::HOST_INSTANCE_ID_ENV).filter(|value|!value.trim().is_empty()).cloned().unwrap_or_else(||crate::protocol_identity::resolve_instance_id(None));
    let settings=crate::host_daemon_state::read_host_settings(&paths)?;
    let policy=resolve_host_policy(&settings.map(serde_json::Value::Object).unwrap_or(serde_json::Value::Null),&env);
    let public_socket=launch.socket.clone();
    let bind_socket=launch.bind_socket.clone().unwrap_or_else(||public_socket.clone());
    let internal=create_internal_socket_path(&paths.dir,platform)?;
    let child_launch=resolve_host_child_launch(&launch,&internal.socket)?;
    let mut child_env=crate::host_successor::successor_env(env.clone(),0,&instance_id,&paths.dir.to_string_lossy(),Some(&agent_dir),&std::collections::HashMap::new());
    child_env.insert(crate::host_watchdog::HOST_PUBLIC_SOCKET_ENV.into(),public_socket.clone());
    if let Some(dir)=&internal.dir{child_env.insert(crate::host_watchdog::HOST_SCRATCH_DIR_ENV.into(),dir.to_string_lossy().into_owned());}
    // Lifetime binding: this supervisor owns the write end of a pipe the child reads, so its EOF
    // means this process died for ANY reason, including a SIGKILL that runs no handler. The ppid
    // fallback covers a descriptor that could not be inherited.
    child_env.insert(crate::host_watchdog::HOST_WATCH_PPID_ENV.into(),std::process::id().to_string());
    let mut cleanup_paths:Vec<String>=Vec::new();
    if launch.bind_socket.is_none(){
        let generation=crate::host_daemon_paths::generation_paths(&paths,&instance_id);
        cleanup_paths.push(paths.pointer_file.to_string_lossy().into_owned());
        cleanup_paths.push(generation.pid_file.to_string_lossy().into_owned());
        cleanup_paths.push(paths.settings_file.to_string_lossy().into_owned());
    }
    child_env.insert(crate::host_watchdog::HOST_CLEANUP_PATHS_ENV.into(),cleanup_paths.join("\n"));
    #[cfg(unix)]
    let _watch_guard=bind_child_watch_pipe(&mut child_env)?;
    let stderr={
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(&paths.stderr_log)?
    };
    let spawnable=spawnable_child_launch(&child_launch,platform);
    let mut child=tokio::process::Command::new(&spawnable.command).args(&spawnable.args).env_clear().envs(child_env).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(stderr).process_group(0).spawn()?;
    let child_pid=child.id().ok_or_else(||std::io::Error::other("supervised host has no process identity"))?;
    // Readiness: the internal hop must answer before the public name is published, or a client
    // that connects during startup would be proxied to nothing.
    let deadline=tokio::time::Instant::now()+std::time::Duration::from_millis(30_000);
    let started=loop{
        if let Some(protocol)=crate::host_probe::probe_protocol_info(&internal.socket,2000).await&&protocol.instance_id.as_deref()==Some(instance_id.as_str()){break protocol;}
        if child.try_wait()?.is_some(){return Err(std::io::Error::other("supervised host exited before ready"));}
        if tokio::time::Instant::now()>=deadline{let _=child.kill().await;return Err(std::io::Error::new(std::io::ErrorKind::TimedOut,"supervised host did not become ready"));}
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    let _=started;
    crate::host_daemon_registration::write_host_registration(&paths,&crate::host_daemon_registration::HostRegistration{pid:child_pid,process_start_time:crate::host_reservations::read_process_start_time(child_pid),socket:public_socket.clone(),instance_id:instance_id.clone(),generation:0.,launch_profile_id:String::new()}).map_err(|error|error.source)?;
    // Publish the public endpoint: an ordinary start binds the public name directly; a successor
    // binds its own generation name and adopts the public one by rename.
    let listener=tokio::net::UnixListener::bind(&bind_socket)?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bind_socket,std::fs::Permissions::from_mode(0o600))?;
    }
    if launch.bind_socket.is_some()&&bind_socket!=public_socket{std::fs::rename(&bind_socket,&public_socket)?;}
    let bound=if launch.bind_socket.is_some(){crate::socket_ownership::stat_socket_identity(std::path::Path::new(&public_socket))?}else{crate::socket_ownership::stat_socket_identity(std::path::Path::new(&bind_socket))?};
    // Publish the public entry's ownership token in this supervisor's private directory (senpi
    // `supervisorPublicOwnerFile`). The host reads it before the watchdog removes that directory, so
    // its crash-path shutdown can unlink the public socket only while it still matches this generation.
    if let Some(dir)=&internal.dir&&let Some(identity)=bound{let _=crate::socket_ownership::write_socket_identity_file(&dir.join(crate::socket_ownership::PUBLIC_SOCKET_IDENTITY_FILE),identity);}
    // Observer: the supervisor cannot see turns directly - it proxies the public socket - so the
    // idle decision reads them from one always-on connection to the internal hop.
    let turns=std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (activity_tx,activity_rx)=tokio::sync::watch::channel(HostActivity::default());
    let (draining_tx,draining_rx)=tokio::sync::watch::channel(false);
    let (connections_tx,connections_rx)=tokio::sync::watch::channel(0usize);
    let observer_busy=turns.clone();
    let observer_socket=internal.socket.clone();
    let observer_draining=draining_rx.clone();
    let observer=tokio::spawn(async move{
        let mut observed=ObservedHostTurns::default();
        loop{
            if *observer_draining.borrow(){return;}
            if let Ok(mut stream)=tokio::net::UnixStream::connect(&observer_socket).await{
                let busy=observer_busy.clone();
                let _=observed.read_events(&mut stream,|observed|{busy.store(observed.busy_turns(),std::sync::atomic::Ordering::SeqCst);}).await;
            }
            if *observer_draining.borrow(){return;}
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    });
    let activity_turns=turns.clone();
    let activity_draining=draining_rx.clone();
    let publisher=tokio::spawn(async move{
        let mut connections_rx=connections_rx;
        let mut draining_watch=activity_draining.clone();
        loop{
            if *activity_draining.borrow(){return;}
            let connections=*connections_rx.borrow_and_update() as u64;
            let active_turns=activity_turns.load(std::sync::atomic::Ordering::SeqCst);
            activity_tx.send_replace(HostActivity{connections,active_turns});
            tokio::select!{
                changed=connections_rx.changed()=>{if changed.is_err(){return;}},
                ()=tokio::time::sleep(std::time::Duration::from_millis(250))=>{},
                changed=draining_watch.changed()=>{if changed.is_err()||*activity_draining.borrow(){return;}},
            }
        }
    });
    let proxy=run_socket_proxy(listener,std::path::PathBuf::from(&internal.socket),None,None,draining_rx.clone(),connections_tx);
    tokio::pin!(proxy);
    let idle=wait_for_idle_exit(policy.idle_exit_ms,activity_rx);
    tokio::pin!(idle);
    tokio::select!{
        result=&mut proxy=>{result?;},
        result=&mut idle=>{let _=result;},
    }
    // Teardown: stop accepting, let the host exit, then remove only the entries this generation
    // still owns so a replacement's freshly published socket survives.
    draining_tx.send_replace(true);
    publisher.abort();observer.abort();
    crate::host_stop::signal_generation(child_pid,rustix::process::Signal::TERM).ok();
    let deadline=tokio::time::Instant::now()+std::time::Duration::from_secs(5);
    loop{
        if child.try_wait()?.is_some(){break;}
        if tokio::time::Instant::now()>=deadline{let _=child.kill().await;let _=child.wait().await;break;}
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    crate::socket_ownership::unlink_owned_socket(&public_socket,bound,platform,|_|{});
    crate::host_daemon_registration::release_generation(&paths,&instance_id,child_pid)?;
    if let Some(dir)=internal.dir{let _=std::fs::remove_dir_all(dir);}
    Ok(())
}
#[derive(Debug,PartialEq)]
pub struct HostLifecyclePolicy{pub cold_start:String,pub idle_exit_ms:f64}
/// The private directory a supervisor's internal hop lives in (senpi `createInternalSocketPath`).
#[derive(Debug,Clone,PartialEq)]
pub struct InternalSocketPath{pub socket:String,pub dir:Option<std::path::PathBuf>,pub secret_path:Option<std::path::PathBuf>}
fn random_short_id()->String{uuid::Uuid::new_v4().simple().to_string()[..8].to_owned()}
/// A supervisor's own internal socket: private against other local users, and independently
/// named so it can never collide with the public one it proxies.
pub fn create_internal_socket_path(base_dir:&std::path::Path,platform:&str)->std::io::Result<InternalSocketPath>{
    if platform=="win32"{
        let dir=base_dir.join(format!("internal-{}",random_short_id()));
        std::fs::DirBuilder::new().recursive(true).create(&dir)?;crate::host_daemon_paths::create_private_directory(&dir)?;
        return Ok(InternalSocketPath{socket:format!("\\\\.\\pipe\\senpi-rpc-internal-{}",random_short_id()),dir:Some(dir.clone()),secret_path:Some(dir.join("secret"))});
    }
    let dir=std::env::temp_dir().join(format!("senpi-rpc-host-internal-{}",random_short_id()));
    crate::host_daemon_paths::create_private_directory(&dir)?;
    let created_at=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0,|elapsed|elapsed.as_millis() as u64);
    let owner=serde_json::json!({"pid":std::process::id(),"processStartTime":crate::host_reservations::read_process_start_time(std::process::id()),"createdAt":created_at});
    crate::host_daemon_state::write_state_file(&dir.join(".owner"),&owner).map_err(|error|error.source)?;
    Ok(InternalSocketPath{socket:dir.join("host.sock").to_string_lossy().into_owned(),dir:Some(dir.clone()),secret_path:Some(dir.join(".secret"))})
}
/// The committed CLI entry this supervisor wraps; the native binary is its own entry.
pub fn resolve_cli_main_path()->std::io::Result<std::path::PathBuf>{std::env::current_exe()}
/// The host child spawn: an explicit child command is forwarded untouched, otherwise the
/// supervisor re-enters the committed CLI entry with the RPC multi-session flags.
pub fn resolve_host_child_launch(launch:&SupervisorLaunch,internal_socket:&str)->std::io::Result<crate::host_launch::HostLaunch>{
    if let Some(command)=&launch.child_command{
        return Ok(crate::host_launch::HostLaunch{command:std::path::PathBuf::from(command),args:launch.child_args.clone().unwrap_or_default().into_iter().chain(["--listen".to_owned(),format!("unix://{internal_socket}")]).collect()});
    }
    Ok(crate::host_launch::HostLaunch{command:resolve_cli_main_path()?,args:["--mode","rpc","--multi-session","--listen"].into_iter().map(str::to_owned).chain([format!("unix://{internal_socket}")]).chain(launch.host_args.iter().cloned()).collect()})
}
/// A spawn shape that survives a Windows shell (senpi `spawnableChildLaunch`).
#[derive(Debug,Clone,PartialEq,Eq)]
pub struct SpawnableLaunch{pub command:String,pub args:Vec<String>,pub shell:bool}
fn quote_windows_shell_arg(value:&str)->String{
    let mut escaped=String::new();let mut backslashes=0usize;
    for character in value.chars(){match character{'\\'=>{backslashes+=1;escaped.push('\\');},'"'=>{for _ in 0..=backslashes{escaped.push('\\');}backslashes=0;escaped.push('"');},'('|')'|'%'|'!'|'^'|'<'|'>'|'&'|'|'|';'|','=>{escaped.push('^');escaped.push(character);},_=>{backslashes=0;escaped.push(character);}}}
    format!("\"{escaped}\"")
}
pub fn spawnable_child_launch(launch:&crate::host_launch::HostLaunch,platform:&str)->SpawnableLaunch{
    let command=launch.command.to_string_lossy().into_owned();
    let extension=launch.command.extension().map(|extension|extension.to_string_lossy().to_lowercase()).unwrap_or_default();
    if platform!="win32"||(extension!=".cmd"&&extension!=".bat"){return SpawnableLaunch{command,args:launch.args.clone(),shell:false};}
    SpawnableLaunch{command:quote_windows_shell_arg(&command),args:launch.args.iter().map(|arg|quote_windows_shell_arg(arg)).collect(),shell:true}
}
pub fn parse_cold_start(value:Option<&str>)->Option<&str>{value.filter(|value|matches!(*value,"transient"|"persistent"))}
pub fn parse_idle_exit_ms(value:Option<&str>)->Option<f64>{let value=value?.trim();if value.is_empty()||!value.bytes().all(|byte|byte.is_ascii_digit()){return None;}value.parse::<f64>().ok().filter(|number|number.is_finite()&&*number>0.)}
pub fn resolve_host_policy(settings:&serde_json::Value,env:&HashMap<String,String>)->HostLifecyclePolicy{
    let cold_start=parse_cold_start(env.get(HOST_COLD_START_ENV).map(String::as_str)).or_else(||parse_cold_start(settings.get("coldStart").and_then(serde_json::Value::as_str))).unwrap_or("transient").into();
    let setting_idle=settings.get("idleExitMs").and_then(|value|if value.is_string(){value.as_str().map(str::to_owned)}else if value.is_number(){Some(value.to_string())}else{None});
    let idle_exit_ms=parse_idle_exit_ms(env.get(HOST_IDLE_EXIT_MS_ENV).map(String::as_str)).or_else(||parse_idle_exit_ms(setting_idle.as_deref())).unwrap_or(DEFAULT_HOST_IDLE_EXIT_MS);HostLifecyclePolicy{cold_start,idle_exit_ms}
}
#[derive(Debug,PartialEq,Eq)]pub enum IdleExitDecision{Active,Idle,Exit}
pub struct IdleExitDecider{pub idle_exit_ms:f64,idle_since:Option<f64>}
impl IdleExitDecider{
    pub fn new(idle_exit_ms:f64)->Self{Self{idle_exit_ms,idle_since:None}}
    pub fn update(&mut self,connections:u64,active_turns:u64,now:f64)->IdleExitDecision{
        if connections>0||active_turns>0{self.idle_since=None;return IdleExitDecision::Active;}
        if self.idle_exit_ms==f64::INFINITY{return IdleExitDecision::Idle;}
        let since=*self.idle_since.get_or_insert(now);if now-since>=self.idle_exit_ms{IdleExitDecision::Exit}else{IdleExitDecision::Idle}
    }
}
#[cfg(test)]mod tests{
    use super::*;
    #[test]fn supervisor_route_skips_only_complete_injected_prefix(){let args=["--extension","dir",INTERNAL_SUPERVISOR_FLAG,"--socket","sock"].map(str::to_owned);assert_eq!(find_internal_supervisor_args(&args),Some(&args[3..]));assert!(find_internal_supervisor_args(&["--model".into(),INTERNAL_SUPERVISOR_FLAG.into()]).is_none());assert!(find_internal_supervisor_args(&["--extension".into(),INTERNAL_SUPERVISOR_FLAG.into()]).is_none());}
    #[test]fn supervisor_parser_keeps_forwarded_args_and_replacement_identity(){let args=["--socket","sock","--replace","1:2","--child-args","[\"one\"]","--mode","rpc"].map(str::to_owned);let launch=parse_supervisor_args(&args).unwrap();assert_eq!(launch.child_args,Some(vec!["one".into()]));assert_eq!(launch.host_args,vec!["--mode","rpc"]);assert_eq!(launch.replace_identity,Some(crate::socket_ownership::SocketFileIdentity{dev:1,ino:2}));assert!(parse_supervisor_args(&["--socket".into(),"sock".into(),"--child-args".into(),"{".into()]).is_none());}
    #[test]fn continuous_idle_resets_on_activity(){let mut decider=IdleExitDecider::new(100.);assert_eq!(decider.update(0,0,0.),IdleExitDecision::Idle);assert_eq!(decider.update(0,1,99.),IdleExitDecision::Active);assert_eq!(decider.update(0,0,100.),IdleExitDecision::Idle);assert_eq!(decider.update(0,0,200.),IdleExitDecision::Exit);}
    #[test]fn policy_precedence_and_invalid_fallback(){let settings=serde_json::json!({"coldStart":"persistent","idleExitMs":1000});let env=HashMap::from([(HOST_COLD_START_ENV.into(),"invalid".into()),(HOST_IDLE_EXIT_MS_ENV.into()," 2000 ".into())]);assert_eq!(resolve_host_policy(&settings,&env),HostLifecyclePolicy{cold_start:"persistent".into(),idle_exit_ms:2000.});assert!(parse_idle_exit_ms(Some("1e3")).is_none());}
    #[test]fn persistent_never_idle_exits(){assert_eq!(IdleExitDecider::new(f64::INFINITY).update(0,0,100000000.),IdleExitDecision::Idle);}
}
