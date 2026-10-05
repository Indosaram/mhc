use maho_rpc::{session_event_writer::{SessionWriterActor,MAX_SHARED_STDIO_QUEUE_BYTES},session_registry::{SessionCloseState,RpcSessionState},session_teardown::admit_session_close};

#[tokio::test]
async fn rejected_close_does_not_mutate_attachment_ownership(){
    let(writer,_reader)=tokio::io::duplex(1);
    let actor=SessionWriterActor::new(writer);
    let mut entry=SessionCloseState{state:RpcSessionState::Open,attachments:1,retain_on_disconnect:false,reservation_key:None,writing:false};
    let response=serde_json::json!({"type":"response","error":"x".repeat(MAX_SHARED_STDIO_QUEUE_BYTES)});
    assert!(admit_session_close(&actor,"s",&response,&mut entry,false).unwrap().is_none());
    assert_eq!(entry.state,RpcSessionState::Open);assert_eq!(entry.attachments,1);
}

#[tokio::test]
async fn admitted_close_claims_one_finalizer_and_joins_later_closes(){
    let(writer,_reader)=tokio::io::duplex(1);
    let actor=SessionWriterActor::new(writer);
    let mut entry=SessionCloseState{state:RpcSessionState::Open,attachments:1,retain_on_disconnect:false,reservation_key:None,writing:false};
    let response=serde_json::json!({"type":"response","command":"close_session","success":true});
    let(id,claim)=admit_session_close(&actor,"s",&response,&mut entry,false).unwrap().unwrap();
    assert_eq!(claim.finalizer,Some(true));assert_eq!(entry.state,RpcSessionState::Closing);actor.release_close_response(id);
    let(id,claim)=admit_session_close(&actor,"s",&response,&mut entry,false).unwrap().unwrap();
    assert_eq!(claim.finalizer,Some(false));actor.release_close_response(id);
}

#[tokio::test]
async fn failed_close_claim_releases_debt_without_sealing_the_writer(){
    let(writer,_reader)=tokio::io::duplex(1);
    let actor=SessionWriterActor::new(writer);
    let mut entry=SessionCloseState{state:RpcSessionState::Closed,attachments:0,retain_on_disconnect:false,reservation_key:None,writing:false};
    let response=serde_json::json!({"type":"response","command":"close_session","success":true});
    assert!(admit_session_close(&actor,"s",&response,&mut entry,false).is_err());
    let id=actor.reserve_close_response("s",&response,true).unwrap().unwrap();actor.release_close_response(id);
    assert_eq!(entry.state,RpcSessionState::Closed);assert_eq!(entry.attachments,0);
}

#[tokio::test]
async fn close_router_checks_connection_ownership_before_reserving_or_detaching(){
    let(writer,_reader)=tokio::io::duplex(1);let actor=SessionWriterActor::new(writer);
    let mut attachments=maho_rpc::session_command_router::ConnectionAttachments::default();attachments.attach("owner","s");
    let mut entry=SessionCloseState{state:RpcSessionState::Open,attachments:1,retain_on_disconnect:false,reservation_key:None,writing:false};
    let response=serde_json::json!({"type":"response","command":"close_session","success":true});
    assert_eq!(attachments.admit_close((Some("foreign"),"s"),&response,&mut entry,false,&actor).err().unwrap(),"unknown_session");
    assert_eq!(entry.attachments,1);assert!(attachments.owns(Some("owner"),"s"));
    let(id,claim)=attachments.admit_close((Some("owner"),"s"),&response,&mut entry,false,&actor).unwrap().unwrap();
    assert_eq!(claim.finalizer,Some(true));assert!(!attachments.owns(Some("owner"),"s"));actor.release_close_response(id);
}
