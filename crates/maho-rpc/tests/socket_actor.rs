use maho_rpc::{loop_blocked_time::LoopBlockedTime,socket_event_fanout::{QueueEntry,SocketEventSinkActor}};
use std::sync::{Arc,Mutex};
use tokio::io::AsyncReadExt;

#[tokio::test]
async fn admission_is_independent_of_peer_drain_and_flush_preserves_fifo(){
    let (writer,mut reader)=tokio::io::duplex(1);
    let written=Arc::new(Mutex::new(Vec::new()));
    let mut actor=SocketEventSinkActor::new(writer,1024,1000,Arc::new(Mutex::new(LoopBlockedTime::default())),|error|panic!("{error}"));
    for line in ["first\n","second\n"]{
        let captured=written.clone();let content=line.to_owned();
        actor.enqueue(QueueEntry{line:content.clone(),key:None,demoted_line:None,on_written:Some(Box::new(move||captured.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(content)))}).unwrap();
    }
    let drain=async{let mut bytes=[0;13];reader.read_exact(&mut bytes).await.unwrap();assert_eq!(&bytes,b"first\nsecond\n");};
    let (result,())=tokio::time::timeout(std::time::Duration::from_secs(2),async{tokio::join!(actor.flush(),drain)}).await.unwrap();
    result.unwrap();assert_eq!(*written.lock().unwrap(),["first\n","second\n"]);
    actor.close();
}

#[tokio::test(start_paused=true)]
async fn stalled_peer_fails_flush_at_the_served_deadline(){
    let (writer,_reader)=tokio::io::duplex(1);
    let blocked=Arc::new(Mutex::new(LoopBlockedTime::default()));
    let (failed_tx,failed_rx)=tokio::sync::oneshot::channel();
    let failed=Mutex::new(Some(failed_tx));
    let mut actor=SocketEventSinkActor::new(writer,1024,100,blocked.clone(),move|error|{if let Some(sender)=failed.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take(){let _=sender.send(error);}});
    actor.enqueue(QueueEntry{line:"blocked".into(),key:None,demoted_line:None,on_written:None}).unwrap();
    let start=tokio::time::Instant::now();
    let error=failed_rx.await.unwrap();
    assert_eq!(start.elapsed(),std::time::Duration::from_millis(100));
    assert!(error.contains("7 queued bytes within 100ms"));
    assert!(actor.flush().await.is_err());
}

#[tokio::test]
async fn overflow_is_retained_by_flush_and_closed_admission_is_inert(){
    let(writer,_reader)=tokio::io::duplex(1);
    let mut actor=SocketEventSinkActor::new(writer,1,100,Arc::new(Mutex::new(LoopBlockedTime::default())),|_|{});
    assert!(actor.enqueue(QueueEntry{line:"too large".into(),key:None,demoted_line:None,on_written:None}).is_err());
    actor.enqueue(QueueEntry{line:"ignored".into(),key:None,demoted_line:None,on_written:None}).unwrap();
    assert!(tokio::time::timeout(std::time::Duration::from_secs(2),actor.flush()).await.unwrap().is_err());
}

#[tokio::test]
async fn overflow_notice_drains_before_eof_and_failure_is_reported_once(){
    let(writer,mut reader)=tokio::io::duplex(1);
    let(tx,rx)=tokio::sync::oneshot::channel();let tx=Mutex::new(Some(tx));
    let actor=SocketEventSinkActor::new(writer,1,100,Arc::new(Mutex::new(LoopBlockedTime::default())),move|error|{tx.lock().unwrap().take().unwrap().send(error).unwrap();});
    actor.enqueue(QueueEntry{line:"overflow".into(),key:None,demoted_line:None,on_written:None}).unwrap_err();
    rx.await.unwrap();
    let mut wire=String::new();
    tokio::time::timeout(std::time::Duration::from_secs(2),reader.read_to_string(&mut wire)).await.unwrap().unwrap();
    assert_eq!(wire,maho_rpc::socket_event_fanout::OVERFLOW_NOTICE);
}

#[tokio::test(start_paused=true)]
async fn stalled_record_finishes_before_notice_when_reader_returns(){
    let(writer,mut reader)=tokio::io::duplex(1);
    let(tx,rx)=tokio::sync::oneshot::channel();let tx=Mutex::new(Some(tx));
    let actor=SocketEventSinkActor::new(writer,1024,100,Arc::new(Mutex::new(LoopBlockedTime::default())),move|error|{tx.lock().unwrap().take().unwrap().send(error).unwrap();});
    actor.enqueue(QueueEntry{line:"record\n".into(),key:None,demoted_line:None,on_written:None}).unwrap();
    rx.await.unwrap();
    let mut wire=String::new();
    reader.read_to_string(&mut wire).await.unwrap();
    assert_eq!(wire,format!("record\n{}",maho_rpc::socket_event_fanout::STALL_NOTICE));
}

#[tokio::test(start_paused=true)]
async fn host_blocked_time_extends_the_live_actor_deadline(){
    let(writer,mut reader)=tokio::io::duplex(1);
    let blocked=Arc::new(Mutex::new(LoopBlockedTime::default()));
    let(tx,rx)=tokio::sync::oneshot::channel();let tx=Mutex::new(Some(tx));
    let actor=SocketEventSinkActor::new(writer,1024,100,blocked.clone(),move|error|{tx.lock().unwrap().take().unwrap().send(error).unwrap();});
    let start=tokio::time::Instant::now();
    actor.enqueue(QueueEntry{line:"record\n".into(),key:None,demoted_line:None,on_written:None}).unwrap();
    // Receiving the first byte proves that the actor installed its drain deadline.
    assert_eq!(reader.read_u8().await.unwrap(),b'r');
    blocked.lock().unwrap().record_loop_blocked_ms(50.);
    rx.await.unwrap();
    assert_eq!(start.elapsed(),std::time::Duration::from_millis(150));
}

#[tokio::test]
async fn close_releases_flush_and_a_blocked_transport_without_waiting_for_deadline(){
    let(writer,mut reader)=tokio::io::duplex(1);
    let mut actor=SocketEventSinkActor::new(writer,1024,30000,Arc::new(Mutex::new(LoopBlockedTime::default())),|error|panic!("{error}"));
    actor.enqueue(QueueEntry{line:"record\n".into(),key:None,demoted_line:None,on_written:None}).unwrap();
    assert_eq!(reader.read_u8().await.unwrap(),b'r');
    actor.close();
    tokio::time::timeout(std::time::Duration::from_secs(2),actor.flush()).await.unwrap().unwrap();
    let mut remaining=Vec::new();
    tokio::time::timeout(std::time::Duration::from_secs(2),reader.read_to_end(&mut remaining)).await.unwrap().unwrap();
}
