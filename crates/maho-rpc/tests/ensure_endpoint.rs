#[tokio::test]
async fn endpoint_ownership_depends_on_acceptance_not_a_protocol_response(){
    let temp=tempfile::tempdir().unwrap();let path=temp.path().join("host.sock");
    assert!(!maho_rpc::host_ensure::public_endpoint_accepts(path.to_str().unwrap()).await);
    let listener=tokio::net::UnixListener::bind(&path).unwrap();
    let accept=async{let(socket,_)=listener.accept().await.unwrap();let mut byte=[0];let _=socket.readable().await;let _=socket.try_read(&mut byte);};
    let(probe,())=tokio::join!(maho_rpc::host_ensure::public_endpoint_accepts(path.to_str().unwrap()),accept);assert!(probe);
    drop(listener);
    assert!(!maho_rpc::host_ensure::public_endpoint_accepts(path.to_str().unwrap()).await);
    assert!(maho_rpc::host_ensure::public_endpoint_accepts("\0abstract").await);
}

#[tokio::test]
async fn early_spawn_failure_reaps_child_and_removes_its_registration(){
    use maho_rpc::host_decision::{HostDecisionClient,HostDecisionPolicy,REQUIRED_HOST_CAPABILITIES};
    let temp=tempfile::tempdir().unwrap();let path=temp.path().join("host.sock");let socket=path.to_str().unwrap();
    let client=HostDecisionClient{protocol_version:1,required_capabilities:REQUIRED_HOST_CAPABILITIES.iter().map(|value|(*value).into()).collect(),identity:maho_core::engine_build_identity::engine_build_identity().clone(),launch_profile:None,started_by_us:false,platform:"linux".into()};
    let mut prepared=maho_rpc::host_ensure::prepare_ensure_host(socket,temp.path(),client,HostDecisionPolicy::Never).await.unwrap();
    let launch=maho_rpc::host_launch::HostLaunch{command:"/usr/bin/false".into(),args:vec![]};
    let settings=maho_rpc::host_daemon_state::HostDaemonSettings{socket:socket.into(),capabilities:vec![],cold_start:"transient".into(),idle_exit_ms:1000.,generation:0.,instance_id:"failed-native".into()};
    let result=maho_rpc::host_ensure::start_host(&prepared,socket,&launch,maho_rpc::host_ensure::HostStartOptions{env:std::env::vars().collect(),settings,launch_profile_id:"profile".into(),timeout:std::time::Duration::from_secs(2)}).await;
    assert!(result.is_err());assert!(!prepared.paths.pointer_file.exists());assert!(!prepared.paths.generations_dir.join("failed-native").exists());prepared.lock.release().unwrap();
}

#[tokio::test]
async fn ensure_preflight_probes_under_the_shared_ownership_lock(){
    use tokio::io::{AsyncBufReadExt,AsyncWriteExt};
    use maho_rpc::host_decision::{HostDecisionClient,HostDecisionPolicy,HostDecision,REQUIRED_HOST_CAPABILITIES};
    let temp=tempfile::tempdir().unwrap();let path=temp.path().join("host.sock");let listener=tokio::net::UnixListener::bind(&path).unwrap();
    let client=HostDecisionClient{protocol_version:1,required_capabilities:REQUIRED_HOST_CAPABILITIES.iter().map(|value|(*value).into()).collect(),identity:maho_core::engine_build_identity::engine_build_identity().clone(),launch_profile:None,started_by_us:false,platform:"linux".into()};
    let serve=async{
        let(socket,_)=listener.accept().await.unwrap();let(read,mut write)=socket.into_split();let mut lines=tokio::io::BufReader::new(read).lines();let request:serde_json::Value=serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        let reply=serde_json::json!({"id":request["id"],"type":"response","command":"get_protocol_info","success":true,"data":{"protocolVersion":1,"serverVersion":"native","capabilities":REQUIRED_HOST_CAPABILITIES}});
        write.write_all(maho_rpc::jsonl::serialize_json_line(&reply).unwrap().as_bytes()).await.unwrap();
    };
    let(prepared,())=tokio::join!(maho_rpc::host_ensure::prepare_ensure_host(path.to_str().unwrap(),temp.path(),client,HostDecisionPolicy::Never),serve);
    let mut prepared=prepared.unwrap();assert!(matches!(prepared.decision,HostDecision::Reuse{..}));assert!(prepared.protocol.is_some());
    let immediate=maho_rpc::ownership_safe_lock::LockRetries{retries:0,min_timeout:std::time::Duration::ZERO,max_timeout:std::time::Duration::ZERO};
    assert!(maho_rpc::ownership_safe_lock::acquire_ownership_safe_lock(&prepared.paths.lock_file,immediate).await.is_err());
    std::fs::write(&prepared.paths.legacy_pid_file,serde_json::json!({"pid":std::process::id(),"processStartTime":maho_rpc::host_reservations::read_process_start_time(std::process::id())}).to_string()).unwrap();
    assert_eq!(maho_rpc::host_ensure::start_refusal(&prepared,path.to_str().unwrap()).await.unwrap(),Some("legacy_host"));
    assert!(prepared.paths.legacy_pid_file.exists());prepared.lock.release().unwrap();
}

#[tokio::test]
async fn managed_stop_never_signals_a_live_process_with_unknown_identity(){
    let owner=maho_rpc::host_daemon_registration::RegisteredHost{pid:std::process::id(),process_start_time:None,socket:None,instance_id:"unknown".into(),generation:0.,writer:None};
    assert!(maho_rpc::host_ensure::stop_managed_host(&owner,std::time::Duration::ZERO).await.is_err());
}

#[tokio::test]
async fn host_runner_reuse_reports_identity_without_recording_spawn_environment(){
    use tokio::io::{AsyncBufReadExt,AsyncWriteExt};
    use maho_rpc::host_decision::{HostDecisionClient,HostDecisionPolicy,REQUIRED_HOST_CAPABILITIES};
    let temp=tempfile::tempdir().unwrap();let path=temp.path().join("host.sock");let socket=path.to_str().unwrap();let listener=tokio::net::UnixListener::bind(&path).unwrap();
    let serve=async{
        for _ in 0..3{
            let(stream,_)=listener.accept().await.unwrap();let(read,mut write)=stream.into_split();let mut lines=tokio::io::BufReader::new(read).lines();let request:serde_json::Value=serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            let reply=serde_json::json!({"type":"response","id":request["id"],"command":"get_protocol_info","success":true,"data":{"protocolVersion":1,"serverVersion":"native","capabilities":REQUIRED_HOST_CAPABILITIES,"instanceId":"existing","generation":2}});
            write.write_all(maho_rpc::jsonl::serialize_json_line(&reply).unwrap().as_bytes()).await.unwrap();
        }
    };
    let client=HostDecisionClient{protocol_version:1,required_capabilities:REQUIRED_HOST_CAPABILITIES.iter().map(|value|(*value).into()).collect(),identity:maho_core::engine_build_identity::engine_build_identity().clone(),launch_profile:None,started_by_us:false,platform:"linux".into()};
    let request=maho_rpc::host_runner::EnsureRequest{client,policy:HostDecisionPolicy::Never,launch:maho_rpc::host_launch::HostLaunch{command:"/usr/bin/false".into(),args:vec![]},start:maho_rpc::host_ensure::HostStartOptions{env:Default::default(),settings:maho_rpc::host_daemon_state::HostDaemonSettings{socket:socket.into(),capabilities:vec![],cold_start:"transient".into(),idle_exit_ms:1000.,generation:0.,instance_id:"unused".into()},launch_profile_id:"profile".into(),timeout:std::time::Duration::from_secs(2)},handoff:maho_rpc::host_handoff::HandoffOptions{host_args:vec![],policy:None,env:Default::default(),launch_profile_id:"profile".into(),readiness_ms:2000},env_keys:vec!["PATH".into()]};
    let(result,())=tokio::time::timeout(std::time::Duration::from_secs(5),async{tokio::join!(maho_rpc::host_runner::ensure_outcome(socket,temp.path(),request),serve)}).await.unwrap();let result=result.unwrap();
    assert_eq!(result.exit_code,0);assert_eq!(result.payload["action"],"reuse");assert_eq!(result.payload["instanceId"],"existing");assert_eq!(result.payload["generation"],2.);assert_eq!(result.payload["reused"],true);
    let paths=maho_rpc::host_daemon_paths::create_host_daemon_paths(socket,temp.path());assert!(!paths.dir.join("env-keys.json").exists());
}
