use maho_ext_mcp::startup_race::*;
use std::time::Duration;
#[test]
fn timeout_override_matches_numeric_precedence() {
    for (raw,expected) in [(" 0 ",0.0),("1e3",1000.0),("0x10",16.0),("0b10",2.0),("0o10",8.0)] {assert_eq!(resolve_mcp_startup_timeout_ms(Some(50.0),Some(raw)),expected);}
    for raw in ["", "  ","bad","-1","Infinity","NaN"] {assert_eq!(resolve_mcp_startup_timeout_ms(Some(50.0),Some(raw)),50.0);}
    assert_eq!(resolve_mcp_startup_timeout_ms(None,None),250.0);
}
#[tokio::test(start_paused=true)]
async fn deadline_does_not_cancel_background_connection() {
    let (send,receive)=tokio::sync::oneshot::channel();
    let connection=tokio::spawn(async move {receive.await.unwrap();Ok::<_,String>(())});
    let mut connection=connection;
    let result=wait_for_mcp_startup_race(async {(&mut connection).await.unwrap()},Duration::from_millis(250)).await.unwrap();
    assert_eq!(result,McpStartupRaceResult::Timeout);assert!(!connection.is_finished());
    send.send(()).unwrap();connection.await.unwrap().unwrap();
}
#[tokio::test(start_paused=true)]
async fn settled_failure_is_not_hidden() {
    assert_eq!(wait_for_mcp_startup_race(async {Err::<(),_>("connect failed")},Duration::from_millis(250)).await,Err("connect failed"));
    assert_eq!(wait_for_mcp_startup_race(async {Ok::<_,()>(())},Duration::from_millis(250)).await,Ok(McpStartupRaceResult::Settled));
}
#[tokio::test(start_paused=true)]
async fn deferred_attach_timeout_preserves_the_completion_signal() {
    let deferred=McpDeferredAttach::default();
    let (send,receive)=tokio::sync::oneshot::channel();
    deferred.track(async move {receive.await.unwrap();});
    assert_eq!(deferred.wait(Duration::from_millis(250)).await,McpStartupRaceResult::Timeout);
    send.send(()).unwrap();
    assert_eq!(deferred.wait(Duration::from_secs(5)).await,McpStartupRaceResult::Settled);
    assert_eq!(deferred.wait(Duration::ZERO).await,McpStartupRaceResult::Settled);
}
#[tokio::test(start_paused=true)]
async fn clearing_deferred_attach_does_not_cancel_the_connect() {
    let deferred=McpDeferredAttach::default();
    let (send,receive)=tokio::sync::oneshot::channel();
    let (finished,completion)=tokio::sync::oneshot::channel();
    deferred.track(async move {receive.await.unwrap();finished.send(()).unwrap();});
    deferred.clear();
    assert_eq!(deferred.wait(Duration::ZERO).await,McpStartupRaceResult::Settled);
    send.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5),completion).await.unwrap().unwrap();
}
