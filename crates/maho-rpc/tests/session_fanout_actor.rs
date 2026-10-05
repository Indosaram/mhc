use maho_rpc::{session_event_fanout::SessionEventFanout,socket_event_fanout::SocketEventSinkActor,loop_blocked_time::LoopBlockedTime};
use std::sync::{Arc,Mutex};
use tokio::io::AsyncReadExt;

#[test]
fn absent_socket_connections_return_the_stdio_destination_record(){
    let mut fanout=SessionEventFanout::default();
    let records=fanout.deliver("s",None,false,&serde_json::json!({"type":"agent_idle"})).unwrap();
    assert_eq!(records,vec![serde_json::json!({"type":"agent_idle","sessionId":"s"})]);
    assert!(fanout.deliver("s",Some("gone"),true,&serde_json::json!({"type":"response"})).unwrap().is_empty());
    let host=serde_json::json!({"type":"host_memory_pressure","rssMb":21,"sessions":1});
    assert_eq!(fanout.broadcast_host_record(&host).unwrap(),Some(host));
}

#[tokio::test]
async fn incomplete_delta_metadata_does_not_demote_a_queued_snapshot(){
    let mut fanout=SessionEventFanout::default();
    let(writer,mut reader)=tokio::io::duplex(1);
    fanout.register("peer",SocketEventSinkActor::new(writer,4096,30000,Arc::new(Mutex::new(LoopBlockedTime::default())),|error|panic!("{error}")));
    fanout.attach("peer","s",0.).unwrap();
    let incomplete=serde_json::json!({"type":"message_update","message":{"role":"assistant"},"assistantMessageEvent":{"type":"text_delta","delta":"a"}});
    let complete=serde_json::json!({"type":"message_update","message":{"role":"assistant"},"assistantMessageEvent":{"type":"text_delta","delta":"b","contentIndex":0}});
    let mut expected=String::new();
    for record in [incomplete,complete]{
        fanout.deliver("s",None,false,&record).unwrap();
        let mut tagged=record;tagged["sessionId"]="s".into();expected.push_str(&maho_rpc::jsonl::serialize_json_line(&tagged).unwrap());
    }
    let read=async{let mut bytes=vec![0;expected.len()];reader.read_exact(&mut bytes).await.unwrap();assert_eq!(bytes,expected.as_bytes());};
    let(result,())=tokio::time::timeout(std::time::Duration::from_secs(2),async{tokio::join!(fanout.flush_connection("peer"),read)}).await.unwrap();result.unwrap();
    fanout.unregister("peer");
}

#[tokio::test]
async fn attached_peer_replays_then_receives_live_records_while_other_peer_stalls(){
    let blocked=Arc::new(Mutex::new(LoopBlockedTime::default()));
    let mut fanout=SessionEventFanout::default();
    let start=serde_json::json!({"type":"message_start","message":{"role":"assistant"}});
    let wire=serde_json::json!({"type":"message_start","message":{"role":"assistant"},"sessionId":"s"});
    fanout.deliver("s",None,false,&start).unwrap();
    let(slow,_slow_reader)=tokio::io::duplex(1);
    fanout.register("slow",SocketEventSinkActor::new(slow,1024,30000,blocked.clone(),|_|{}));
    fanout.attach("slow","s",0.).unwrap();
    let(fast,mut reader)=tokio::io::duplex(1);
    fanout.register("fast",SocketEventSinkActor::new(fast,1024,30000,blocked,|error|panic!("{error}")));
    fanout.attach("fast","s",0.).unwrap();
    fanout.deliver("s",None,false,&serde_json::json!({"type":"message_end"})).unwrap();
    let expected=format!("{}{}",maho_rpc::jsonl::serialize_json_line(&wire).unwrap(),maho_rpc::jsonl::serialize_json_line(&serde_json::json!({"type":"message_end","sessionId":"s"})).unwrap());
    let read=async{let mut bytes=vec![0;expected.len()];reader.read_exact(&mut bytes).await.unwrap();assert_eq!(bytes,expected.as_bytes());};
    let(result,())=tokio::time::timeout(std::time::Duration::from_secs(2),async{tokio::join!(fanout.flush_connection("fast"),read)}).await.unwrap();result.unwrap();
    fanout.unregister("slow");fanout.unregister("fast");
}

#[tokio::test]
async fn overflowed_peer_does_not_prevent_delivery_to_the_next_peer(){
    let blocked=Arc::new(Mutex::new(LoopBlockedTime::default()));
    let mut fanout=SessionEventFanout::default();
    let(small,mut cut_reader)=tokio::io::duplex(1);
    fanout.register("small",SocketEventSinkActor::new(small,1,30000,blocked.clone(),|_|{}));
    fanout.attach("small","s",0.).unwrap();
    let(writer,mut reader)=tokio::io::duplex(1);
    fanout.register("healthy",SocketEventSinkActor::new(writer,1024,30000,blocked,|error|panic!("{error}")));
    fanout.attach("healthy","s",0.).unwrap();
    fanout.deliver("s",None,false,&serde_json::json!({"type":"agent_idle"})).unwrap();
    let expected=maho_rpc::jsonl::serialize_json_line(&serde_json::json!({"type":"agent_idle","sessionId":"s"})).unwrap();
    let read=async{let mut bytes=vec![0;expected.len()];reader.read_exact(&mut bytes).await.unwrap();assert_eq!(bytes,expected.as_bytes());};
    let(result,())=tokio::time::timeout(std::time::Duration::from_secs(2),async{tokio::join!(fanout.flush_connection("healthy"),read)}).await.unwrap();result.unwrap();
    assert_eq!(fanout.targets.targets("s",None,false,false,Some("agent_idle")),vec![Some("healthy".into())]);
    let mut notice=String::new();tokio::time::timeout(std::time::Duration::from_secs(2),cut_reader.read_to_string(&mut notice)).await.unwrap().unwrap();
    assert_eq!(notice,maho_rpc::socket_event_fanout::OVERFLOW_NOTICE);
    fanout.unregister("healthy");
}

#[tokio::test]
async fn enabling_rendered_capability_replays_only_rendered_records(){
    let mut fanout=SessionEventFanout::default();
    fanout.deliver("s",None,false,&serde_json::json!({"type":"message_start"})).unwrap();
    fanout.deliver("s",None,false,&serde_json::json!({"type":"custom_component","__senpiRenderedComponent":true,"text":"visible"})).unwrap();
    let(writer,mut reader)=tokio::io::duplex(1);
    fanout.register("client",SocketEventSinkActor::new(writer,1024,30000,Arc::new(Mutex::new(LoopBlockedTime::default())),|error|panic!("{error}")));
    fanout.attach("client","s",0.).unwrap();
    fanout.set_capabilities("client",&["rendered_components".into()]).unwrap();
    fanout.set_capabilities("client",&["rendered_components".into()]).unwrap();
    let expected=[serde_json::json!({"type":"message_start","sessionId":"s"}),serde_json::json!({"type":"custom_component","text":"visible","sessionId":"s"})].iter().map(|value|maho_rpc::jsonl::serialize_json_line(value).unwrap()).collect::<String>();
    let read=async{let mut bytes=vec![0;expected.len()];reader.read_exact(&mut bytes).await.unwrap();assert_eq!(bytes,expected.as_bytes());};
    let(result,())=tokio::time::timeout(std::time::Duration::from_secs(2),async{tokio::join!(fanout.flush_connection("client"),read)}).await.unwrap();result.unwrap();
    fanout.unregister("client");
    let mut remaining=Vec::new();reader.read_to_end(&mut remaining).await.unwrap();assert!(remaining.is_empty());
}

#[tokio::test]
async fn worker_close_lifecycle_reaches_only_attached_connections(){
    let mut fanout=SessionEventFanout::default();
    let blocked=Arc::new(Mutex::new(LoopBlockedTime::default()));
    let(owner,mut owner_reader)=tokio::io::duplex(1);
    fanout.register("owner",SocketEventSinkActor::new(owner,1024,30000,blocked.clone(),|error|panic!("{error}")));
    let(observer,mut observer_reader)=tokio::io::duplex(1);
    fanout.register("observer",SocketEventSinkActor::new(observer,1024,30000,blocked,|error|panic!("{error}")));
    fanout.attach("owner","s",0.).unwrap();
    fanout.set_session_kind("s",maho_ext_api::SessionKind::Worker);
    fanout.deliver("s",None,false,&serde_json::json!({"type":"session_closed"})).unwrap();
    let expected=maho_rpc::jsonl::serialize_json_line(&serde_json::json!({"type":"session_closed","sessionId":"s"})).unwrap();
    let read=async{let mut bytes=vec![0;expected.len()];owner_reader.read_exact(&mut bytes).await.unwrap();assert_eq!(bytes,expected.as_bytes());};
    let(result,())=tokio::time::timeout(std::time::Duration::from_secs(2),async{tokio::join!(fanout.flush_connection("owner"),read)}).await.unwrap();result.unwrap();
    fanout.unregister("owner");fanout.unregister("observer");
    let mut bytes=Vec::new();tokio::time::timeout(std::time::Duration::from_secs(2),observer_reader.read_to_end(&mut bytes)).await.unwrap().unwrap();assert!(bytes.is_empty());
}
