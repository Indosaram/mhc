#[tokio::test]
async fn writer_actor_admits_more_sessions_while_peer_is_blocked(){
    use tokio::io::AsyncReadExt;
    let(writer,mut reader)=tokio::io::duplex(1);
    let actor=maho_rpc::session_event_writer::SessionWriterActor::new(writer);
    actor.enqueue("a",serde_json::json!({"n":1})).unwrap();
    actor.enqueue("a",serde_json::json!({"n":2})).unwrap();
    actor.enqueue("b",serde_json::json!({"n":3})).unwrap();
    let read=async{let mut bytes=[0;24];reader.read_exact(&mut bytes).await.unwrap();assert_eq!(&bytes,b"{\"n\":1}\n{\"n\":3}\n{\"n\":2}\n");};
    let(result,())=tokio::time::timeout(std::time::Duration::from_secs(2),async{tokio::join!(actor.flush(),read)}).await.unwrap();result.unwrap();
}

#[tokio::test]
async fn transport_drain_preserves_round_robin_record_boundaries(){
    use tokio::io::AsyncReadExt;
    let mut scheduler=maho_rpc::session_event_writer::SessionRecordScheduler::default();
    scheduler.enqueue("a",None,serde_json::json!({"n":1}));
    scheduler.enqueue("a",None,serde_json::json!({"n":2}));
    scheduler.enqueue("b",None,serde_json::json!({"n":3}));
    let (mut writer,mut reader)=tokio::io::duplex(1);
    let run=async{scheduler.drain(&mut writer).await.unwrap();drop(writer);};
    let read=async{let mut wire=String::new();reader.read_to_string(&mut wire).await.unwrap();wire};
    let ((),wire)=tokio::time::timeout(std::time::Duration::from_secs(2),async{tokio::join!(run,read)}).await.unwrap();
    assert_eq!(wire,"{\"n\":1}\n{\"n\":3}\n{\"n\":2}\n");
}

#[tokio::test]
async fn failed_writer_rejects_flush_and_later_admission(){
    let(writer,reader)=tokio::io::duplex(1);drop(reader);
    let actor=maho_rpc::session_event_writer::SessionWriterActor::new(writer);
    actor.enqueue("a",serde_json::json!({"n":1})).unwrap();
    assert!(tokio::time::timeout(std::time::Duration::from_secs(2),actor.flush()).await.unwrap().is_err());
    assert!(actor.enqueue("b",serde_json::json!({"n":2})).is_err());
}

#[tokio::test]
async fn reserved_terminal_close_is_final_after_existing_session_records(){
    use tokio::io::AsyncReadExt;
    let(writer,mut reader)=tokio::io::duplex(1);
    let actor=maho_rpc::session_event_writer::SessionWriterActor::new(writer);
    actor.enqueue("s",serde_json::json!({"type":"agent_idle"})).unwrap();
    let response=serde_json::json!({"type":"response","command":"close_session","success":true});
    let id=actor.reserve_close_response("s",&response,true).unwrap().unwrap();
    actor.complete_close_response(id,"s",response.clone(),true,Some("client_close")).unwrap();
    actor.enqueue("s",serde_json::json!({"type":"late_event"})).unwrap();
    let expected=[serde_json::json!({"type":"agent_idle"}),serde_json::json!({"type":"session_closed","sessionId":"s","reason":"client_close"}),serde_json::json!({"type":"response","command":"close_session","success":true,"sessionId":"s"})].iter().map(|value|maho_rpc::jsonl::serialize_json_line(value).unwrap()).collect::<String>();
    let read=async{let mut bytes=vec![0;expected.len()];reader.read_exact(&mut bytes).await.unwrap();assert_eq!(bytes,expected.as_bytes());};
    let(result,())=tokio::time::timeout(std::time::Duration::from_secs(2),async{tokio::join!(actor.flush(),read)}).await.unwrap();result.unwrap();
}
