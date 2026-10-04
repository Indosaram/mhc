use maho_server::app_server::{server_core::ServerCore, stdio::{run_shared_stdio_until, run_stdio}};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

static STDIO_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn explicit_shared_stdio_shutdown_removes_connection_without_input_eof() {
    let _guard = STDIO_TEST_LOCK.lock().await;
    use std::sync::Arc;
    use tokio::sync::RwLock;
    let core=Arc::new(RwLock::new(ServerCore::new("/tmp/home".into(),"1".into(),"Linux".into(),"test".into(),"x64".into(),"linux".into())));
    let (mut client,server)=tokio::io::duplex(8192);
    let (input,output)=tokio::io::split(server);
    let (stop,shutdown)=tokio::sync::oneshot::channel();
    let client_work=async move {
        client.write_all(b"{\"id\":1,\"method\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"c\",\"version\":\"1\"}}}\n").await.unwrap();
        let mut line=Vec::new();let mut byte=[0];
        loop {client.read_exact(&mut byte).await.unwrap();line.push(byte[0]);if byte[0]==b'\n' {break;}}
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&line).unwrap()["id"],1);
        stop.send(()).unwrap();
        assert_eq!(client.read(&mut byte).await.unwrap(),0);
    };
    tokio::time::timeout(std::time::Duration::from_secs(3),async {
        let (result,())=tokio::join!(run_shared_stdio_until(core.clone(),input,output,async {shutdown.await.unwrap()},None),client_work);
        result.unwrap();
    }).await.unwrap();
    assert!(core.read().await.get_connection("stdio").is_none());
}

#[tokio::test]
async fn duplex_transport_orders_parse_error_initialize_and_final_unterminated_request() {
    let mut core = ServerCore::new("/tmp/home".into(), "1".into(), "Linux".into(), "test".into(), "x64".into(), "linux".into());
    let (mut client, server) = tokio::io::duplex(8192);
    let (input, output) = tokio::io::split(server);
    let client_work = async move {
        client.write_all(b"invalid\n{\"id\":1,\"method\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"c\",\"version\":\"1\"}}}\n{\"id\":2,\"method\":\"unknown\"}").await.unwrap();
        client.shutdown().await.unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).await.unwrap();
        response
    };
    let (result, response) = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        tokio::join!(run_stdio(&mut core, "stdio", input, output), client_work)
    }).await.unwrap();
    result.unwrap();
    let messages: Vec<serde_json::Value> = response.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0]["error"]["code"], -32700);
    assert_eq!(messages[1]["id"], 1);
    assert_eq!(messages[1]["result"]["codexHome"], "/tmp/home");
    assert_eq!(messages[2]["id"], 2);
    assert_eq!(messages[2]["error"]["code"], -32601);
    assert!(core.get_connection("stdio").is_none());
}

#[tokio::test]
async fn shared_stdio_rejects_a_second_active_transport_and_restarts_after_close() {
    let _guard = STDIO_TEST_LOCK.lock().await;
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
    use tokio::sync::RwLock;
    tokio::time::timeout(std::time::Duration::from_secs(3),async {
    let core=Arc::new(RwLock::new(ServerCore::new("/tmp/home".into(),"1".into(),"Linux".into(),"test".into(),"x64".into(),"linux".into())));
    let (_first_client,first_server)=tokio::io::duplex(8192);
    let (first_input,first_output)=tokio::io::split(first_server);
    let (stop,shutdown)=tokio::sync::oneshot::channel();
    let reasons=Arc::new(AtomicUsize::new(0));
    let callback=reasons.clone();
    let mut first=Box::pin(run_shared_stdio_until(core.clone(),first_input,first_output,async {shutdown.await.unwrap()},Some(Arc::new(move |_| {callback.fetch_add(1,Ordering::SeqCst);}))));
    assert!(futures_util::poll!(first.as_mut()).is_pending());
    let (idle_input,idle_output)=tokio::io::duplex(8192);
    let second=run_shared_stdio_until(core.clone(),idle_input,idle_output,std::future::pending(),None).await;
    assert_eq!(second.unwrap_err().message,"stdio transport already active");
    stop.send(()).unwrap();
    first.await.unwrap();
    assert_eq!(reasons.load(Ordering::SeqCst),1);
    let (mut restart_client,restart_server)=tokio::io::duplex(8192);
    let (restart_input,restart_output)=tokio::io::split(restart_server);
    let (stop2,shutdown2)=tokio::sync::oneshot::channel();
    let restart_work=async move {
        restart_client.write_all(b"{\"id\":1,\"method\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"c\",\"version\":\"1\"}}}\n").await.unwrap();
        let mut line=Vec::new();let mut byte=[0];
        loop {restart_client.read_exact(&mut byte).await.unwrap();line.push(byte[0]);if byte[0]==b'\n' {break;}}
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&line).unwrap()["id"],1);
        stop2.send(()).unwrap();
    };
    let (result,())=tokio::join!(run_shared_stdio_until(core.clone(),restart_input,restart_output,async {shutdown2.await.unwrap()},None),restart_work);
    result.unwrap();
    assert!(core.read().await.get_connection("stdio").is_none());
    }).await.unwrap();
}

#[tokio::test]
async fn shared_stdio_reports_eof_reason_once_and_cleans_up() {
    let _guard = STDIO_TEST_LOCK.lock().await;
    use std::sync::{Arc, Mutex};
    use tokio::sync::RwLock;
    let core=Arc::new(RwLock::new(ServerCore::new("/tmp/home".into(),"1".into(),"Linux".into(),"test".into(),"x64".into(),"linux".into())));
    let (mut client,server)=tokio::io::duplex(8192);
    let (input,output)=tokio::io::split(server);
    let reasons=Arc::new(Mutex::new(Vec::<String>::new()));
    let recorded=reasons.clone();
    let client_work=async move {
        client.write_all(b"{\"id\":1,\"method\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"c\",\"version\":\"1\"}}}\n").await.unwrap();
        let mut line=Vec::new();let mut byte=[0];
        loop {client.read_exact(&mut byte).await.unwrap();line.push(byte[0]);if byte[0]==b'\n' {break;}}
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&line).unwrap()["id"],1);
        client.shutdown().await.unwrap();
    };
    tokio::time::timeout(std::time::Duration::from_secs(3),async {
        let (result,())=tokio::join!(run_shared_stdio_until(core.clone(),input,output,std::future::pending(),Some(Arc::new(move |reason| {recorded.lock().unwrap().push(reason.to_owned());}))),client_work);
        result.unwrap();
    }).await.unwrap();
    assert_eq!(reasons.lock().unwrap().as_slice(),&["stdin ended".to_owned()]);
    assert!(core.read().await.get_connection("stdio").is_none());
}
