#[tokio::test(start_paused=true)]
async fn installed_idle_timer_exits_after_a_continuous_idle_window(){
    let(_activity,receiver)=tokio::sync::watch::channel(maho_rpc::host_lifecycle::HostActivity::default());
    let start=tokio::time::Instant::now();
    maho_rpc::host_lifecycle::wait_for_idle_exit(100.,receiver).await.unwrap();
    assert_eq!(start.elapsed(),std::time::Duration::from_millis(100));
}

#[tokio::test]
async fn draining_proxy_preserves_attached_connection_and_authenticates_internal_hop(){
    use tokio::io::{AsyncReadExt,AsyncWriteExt};
    let dir=tempfile::tempdir().unwrap();let public=dir.path().join("public");let internal=dir.path().join("internal");
    let listener=tokio::net::UnixListener::bind(&public).unwrap();let backend=tokio::net::UnixListener::bind(&internal).unwrap();
    let(drain,draining)=tokio::sync::watch::channel(false);let(count,mut counts)=tokio::sync::watch::channel(0);
    let proxy=tokio::spawn(maho_rpc::host_lifecycle::run_socket_proxy(listener,internal,Some(vec![1;32]),Some(vec![2;32]),draining,count));
    let scenario=async{
        let mut client=tokio::net::UnixStream::connect(&public).await.unwrap();
        maho_rpc::socket_transport::send_socket_handshake(&mut client,&[1;32]).await.unwrap();
        let(mut host,_)=backend.accept().await.unwrap();
        maho_rpc::socket_transport::authenticate_socket(&mut host,&[2;32]).await.unwrap();
        counts.wait_for(|count|*count==1).await.unwrap();
        drain.send(true).unwrap();
        client.write_all(b"request\n").await.unwrap();client.shutdown().await.unwrap();
        let mut request=String::new();host.read_to_string(&mut request).await.unwrap();assert_eq!(request,"request\n");
        host.write_all(b"final lifecycle\n").await.unwrap();host.shutdown().await.unwrap();
        let mut output=String::new();client.read_to_string(&mut output).await.unwrap();assert_eq!(output,"final lifecycle\n");
        proxy.await.unwrap().unwrap();assert_eq!(*counts.borrow(),0);
    };
    tokio::time::timeout(std::time::Duration::from_secs(2),scenario).await.unwrap();
}
