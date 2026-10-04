#[tokio::test]
async fn failed_successor_preserves_predecessor_registration_and_socket(){
    let temp=tempfile::tempdir().unwrap();let path=temp.path().join("host.sock");let socket=path.to_str().unwrap();let listener=tokio::net::UnixListener::bind(&path).unwrap();
    let paths=maho_rpc::host_daemon_paths::create_host_daemon_paths(socket,temp.path());maho_rpc::host_daemon_paths::create_daemon_directories(&paths).unwrap();
    let pid=std::process::id();maho_rpc::host_daemon_registration::write_host_registration(&paths,&maho_rpc::host_daemon_registration::HostRegistration{pid,process_start_time:maho_rpc::host_reservations::read_process_start_time(pid),socket:socket.into(),instance_id:"old".into(),generation:0.,launch_profile_id:"profile".into()}).unwrap();
    let owner=maho_rpc::host_daemon_registration::read_host_registration(&paths).unwrap().unwrap();
    let host=maho_rpc::host_protocol_info::parse_host_protocol_info(&serde_json::json!({"protocolVersion":1,"serverVersion":"1","capabilities":["generation_handoff"],"instanceId":"old"})).unwrap();
    let context=maho_rpc::host_handoff::HandoffContext{paths,host,owner};let identity=maho_rpc::socket_ownership::stat_socket_identity(&path).unwrap().unwrap();
    let result=maho_rpc::host_successor::start_successor(&context,socket,identity,maho_rpc::host_successor::SuccessorLaunchOptions{launch:maho_rpc::host_launch::HostLaunch{command:"/usr/bin/false".into(),args:vec![]},env:std::env::vars().collect(),instance_id:"new".into(),generation:1,launch_profile_id:"profile".into(),readiness_ms:10}).await.unwrap();
    assert_eq!(result.err().unwrap().reason,"successor_unavailable");
    assert_eq!(maho_rpc::host_daemon_registration::read_host_registration(&context.paths).unwrap().unwrap().instance_id,"old");
    assert_eq!(maho_rpc::socket_ownership::stat_socket_identity(&path).unwrap(),Some(identity));drop(listener);
}
