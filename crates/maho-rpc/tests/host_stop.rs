use maho_rpc::{host_stop::{stop_host,StopHostResult},host_daemon_paths::{create_host_daemon_paths,create_daemon_directories,create_generation_directory,generation_paths}};
use tokio::io::{AsyncReadExt,AsyncWriteExt};
#[tokio::test]
async fn unknown_owner_is_refused_without_creating_shared_state(){let temp=tempfile::tempdir().unwrap();assert_eq!(stop_host("missing",temp.path(),false,true,100).await.unwrap(),StopHostResult::Refuse{reason:"unknown_owner"});assert!(!temp.path().join("rpc-host-daemon").exists());}
#[tokio::test]
async fn live_sessions_refuse_real_stop_and_preserve_registration(){
    let temp=tempfile::tempdir().unwrap();let socket=temp.path().join("host.sock");let socket=socket.to_str().unwrap();let listener=tokio::net::UnixListener::bind(socket).unwrap();
    let paths=create_host_daemon_paths(socket,temp.path());create_daemon_directories(&paths).unwrap();let generation=generation_paths(&paths,"one");create_generation_directory(&generation).unwrap();
    std::fs::write(&paths.pointer_file,r#"{"instance_id":"one"}"#).unwrap();std::fs::write(&generation.pid_file,serde_json::json!({"pid":std::process::id(),"processStartTime":maho_rpc::host_reservations::read_process_start_time(std::process::id()),"socket":socket}).to_string()).unwrap();
    let serve=async{for data in [serde_json::json!({"serverVersion":"1","capabilities":[]}),serde_json::json!({"sessions":[{}]})]{let(mut stream,_)=listener.accept().await.unwrap();let mut byte=[0];loop{stream.read_exact(&mut byte).await.unwrap();if byte[0]==b'\n'{break;}}let response=serde_json::json!({"id":"ensure-host-probe","success":true,"data":data});stream.write_all(maho_rpc::jsonl::serialize_json_line(&response).unwrap().as_bytes()).await.unwrap();}};
    let stop=stop_host(socket,temp.path(),false,false,1000);let(result,())=tokio::time::timeout(std::time::Duration::from_secs(3),async{tokio::join!(stop,serve)}).await.unwrap();assert_eq!(result.unwrap(),StopHostResult::Refuse{reason:"sessions_live"});assert!(generation.pid_file.exists());assert!(paths.pointer_file.exists());
}
