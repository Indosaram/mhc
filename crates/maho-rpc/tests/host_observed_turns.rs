#[test]
fn observer_counts_busy_sessions_and_unknown_grace_not_individual_starts(){
    let mut turns=maho_rpc::host_lifecycle::ObservedHostTurns::default();
    let mut observer=maho_rpc::observer_link::ObserverLink::default();observer.opened(1);
    assert!(!turns.observe("invalid"));assert!(!turns.observe("{\"type\":\"agent_start\"}"));
    for _ in 0..2{assert!(turns.observe("{\"type\":\"agent_start\",\"sessionId\":\"s\"}"));}
    assert_eq!(turns.activity(2,&observer,0.,100.).active_turns,1);
    turns.observe("{\"type\":\"agent_settled\",\"sessionId\":\"s\"}");assert_eq!(turns.activity(0,&observer,0.,100.).active_turns,1);
    turns.observe("{\"type\":\"agent_settled\",\"sessionId\":\"s\"}");assert_eq!(turns.activity(0,&observer,0.,100.).active_turns,0);
    observer.lost(1,false,10.);
    assert_eq!(turns.activity(0,&observer,109.,100.).active_turns,1);
    assert_eq!(turns.activity(0,&observer,110.,100.).active_turns,0);
}

#[tokio::test]
async fn observer_reader_tracks_strict_jsonl_until_transport_eof(){
    use tokio::io::AsyncWriteExt;
    let(mut writer,reader)=tokio::io::duplex(1);
    let mut turns=maho_rpc::host_lifecycle::ObservedHostTurns::default();let mut observer=maho_rpc::observer_link::ObserverLink::default();observer.opened(1);
    let mut activity=Vec::new();
    let read=turns.read_events(reader,|turns|activity.push(turns.activity(0,&observer,0.,100.).active_turns));
    let write=async{writer.write_all(b"invalid\n{\"type\":\"agent_start\",\"sessionId\":\"s\"}\n{\"type\":\"agent_settled\",\"sessionId\":\"s\"}").await.unwrap();writer.shutdown().await.unwrap();};
    let(result,())=tokio::time::timeout(std::time::Duration::from_secs(2),async{tokio::join!(read,write)}).await.unwrap();result.unwrap();
    assert_eq!(activity,vec![1,0]);
}
