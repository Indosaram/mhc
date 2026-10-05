use maho_rpc::session_event_writer::{CloseResponseReservations,MAX_SHARED_STDIO_QUEUE_RECORDS,MAX_SHARED_STDIO_QUEUE_BYTES};

#[test]
fn terminal_close_reserves_lifecycle_and_response_before_mutation(){
    let mut reservations=CloseResponseReservations::default();
    let response=serde_json::json!({"type":"response","id":"close","command":"close_session","success":true});
    assert!(reservations.reserve("s",&response,true,(MAX_SHARED_STDIO_QUEUE_RECORDS-1,0)).unwrap().is_none());
    let id=reservations.reserve("s",&response,true,(MAX_SHARED_STDIO_QUEUE_RECORDS-2,0)).unwrap().unwrap();
    let(records,bytes)=reservations.pending_size();assert_eq!(records,2);assert!(bytes>0);
    assert!(reservations.reserve("s",&response,false,(0,MAX_SHARED_STDIO_QUEUE_BYTES-bytes)).unwrap().is_none());
    assert!(reservations.release(id));assert!(!reservations.release(id));assert_eq!(reservations.pending_size(),(0,0));
}

#[test]
fn joined_close_reserves_only_its_noncompactable_response(){
    let mut reservations=CloseResponseReservations::default();
    let response=serde_json::json!({"type":"response","command":"close_session","success":true});
    let id=reservations.reserve("s",&response,false,(MAX_SHARED_STDIO_QUEUE_RECORDS-1,0)).unwrap().unwrap();
    assert_eq!(reservations.pending_size().0,1);
    assert!(reservations.release(id));
}
