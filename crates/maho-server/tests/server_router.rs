use maho_server::server::{errors::ServerError, session_router::SessionRouter, types::*};
use serde_json::{Value, json};
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

async fn await_invalidation(events: &mut tokio::sync::watch::Receiver<u64>, target: u64) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if *events.borrow_and_update() >= target { return; }
            events.changed().await.expect("invalidation watcher alive");
        }
    }).await.expect("invalidation within deadline");
}

struct Host(Arc<AtomicUsize>);
#[tokio::test]
async fn removal_during_attachment_acquisition_rejects_stale_reopened_handle() {
    use maho_server::server::testing::host::OpenGate;
    struct GatedHost {gate:OpenGate,opens:AtomicUsize,releases:Arc<AtomicUsize>}
    struct GatedHandle {gate:Option<OpenGate>,releases:Arc<AtomicUsize>}
    impl ServerHost for GatedHost {
        fn server_services(&self)->&dyn RoutedServerServiceHost {self}
        fn resolve_session<'a>(&'a self,id:&'a str)->ServerFuture<'a,Value> {Box::pin(async move {Ok(json!({"id":id}))})}
        fn open_session(&self,_:Value)->ServerFuture<'_,Arc<dyn RoutedSessionHandle>> {Box::pin(async move {
            let gate=(self.opens.fetch_add(1,Ordering::SeqCst)==0).then(||self.gate.clone());
            Ok(Arc::new(GatedHandle {gate,releases:self.releases.clone()}) as Arc<dyn RoutedSessionHandle>)
        })}
    }
    impl RoutedServerServiceHost for GatedHost {
        fn attach_client(&self,_:Arc<dyn RoutedServerPresentation>)->ServerFuture<'_,Arc<dyn RoutedServerServiceAttachment>> {Box::pin(async {Err(ServerError::new("internal_error","Unused"))})}
    }
    impl RoutedSessionHandle for GatedHandle {
        fn attach_client(&self)->ServerFuture<'_,Arc<dyn RoutedSessionAttachment>> {Box::pin(async move {
            if let Some(gate)=&self.gate {gate.entered.resolve(());gate.release.wait().await;}
            Ok(Arc::new(Lease(self.releases.clone())) as Arc<dyn RoutedSessionAttachment>)
        })}
        fn close(&self)->ServerFuture<'_,()> {Box::pin(async {Ok(())})}
    }
    tokio::time::timeout(std::time::Duration::from_secs(2),async {
        let gate=OpenGate::default();let releases=Arc::new(AtomicUsize::new(0));
        let router=Arc::new(SessionRouter::new(Arc::new(GatedHost {gate:gate.clone(),opens:AtomicUsize::new(0),releases:releases.clone()}),"00000000-0000-4000-8000-000000000001".into()));
        let first={let router=router.clone();tokio::spawn(async move {router.attach("same").await})};
        gate.entered.wait().await;
        router.remove("same").await.unwrap();
        let replacement=router.attach("same").await.unwrap();
        gate.release.resolve(());
        assert!(matches!(first.await.unwrap(),Err(error) if error.code=="server_draining"));
        assert_eq!(releases.load(Ordering::SeqCst),1);
        replacement.release().await.unwrap();router.close().await.unwrap();
    }).await.unwrap();
}
#[tokio::test]
async fn attachment_release_repeats_failure_without_releasing_twice() {
    struct FailingHost(Arc<AtomicUsize>);
    struct FailingLease(Arc<AtomicUsize>);
    impl ServerHost for FailingHost {
        fn server_services(&self)->&dyn RoutedServerServiceHost {self}
        fn resolve_session<'a>(&'a self,id:&'a str)->ServerFuture<'a,Value> {Box::pin(async move {Ok(json!({"id":id}))})}
        fn open_session(&self,_:Value)->ServerFuture<'_,Arc<dyn RoutedSessionHandle>> {Box::pin(async move {Ok(Arc::new(FailingHost(self.0.clone())) as Arc<dyn RoutedSessionHandle>)})}
    }
    impl RoutedServerServiceHost for FailingHost {
        fn attach_client(&self,_:Arc<dyn RoutedServerPresentation>)->ServerFuture<'_,Arc<dyn RoutedServerServiceAttachment>> {Box::pin(async {Err(ServerError::new("internal_error","Unused"))})}
    }
    impl RoutedSessionHandle for FailingHost {
        fn attach_client(&self)->ServerFuture<'_,Arc<dyn RoutedSessionAttachment>> {Box::pin(async move {Ok(Arc::new(FailingLease(self.0.clone())) as Arc<dyn RoutedSessionAttachment>)})}
        fn close(&self)->ServerFuture<'_,()> {Box::pin(async {Ok(())})}
    }
    impl RoutedSessionAttachment for FailingLease {
        fn invoke_service<'a>(&'a self,_:Value,_:Publisher,_:Context)->ServerFuture<'a,Option<Value>> {Box::pin(async {Ok(None)})}
        fn release(&self)->ServerFuture<'_,()> {Box::pin(async move {self.0.fetch_add(1,Ordering::SeqCst);Err(ServerError::new("internal_error","lease release failed"))})}
    }
    let releases=Arc::new(AtomicUsize::new(0));
    let router=SessionRouter::new(Arc::new(FailingHost(releases.clone())),"00000000-0000-4000-8000-000000000001".into());
    let attachment=router.attach("session").await.unwrap();
    let (first,second)=tokio::join!(attachment.release(),attachment.release());
    assert!(first.is_err());assert_eq!(first,second);assert_eq!(first,attachment.release().await);
    assert_eq!(releases.load(Ordering::SeqCst),1);
    assert!(router.close().await.is_err());
}
#[tokio::test]
async fn concurrent_clients_share_one_session_open() {
    use maho_server::server::testing::TestServerHost;
    let host=Arc::new(TestServerHost::default());host.seed(None,None).await.unwrap();
    let router=SessionRouter::new(host.clone(),"00000000-0000-4000-8000-000000000001".into());
    let (first,second)=tokio::join!(router.attach("session-1"),router.attach("session-1"));
    let first=first.unwrap();let second=second.unwrap();
    assert_eq!(host.state.lock().await.open_session_count,1);
    assert_eq!(host.latest_harness("session-1").await.unwrap().state.lock().await.attached_clients,2);
    first.release().await.unwrap();second.release().await.unwrap();router.close().await.unwrap();
}
#[tokio::test]
async fn independent_session_open_is_not_blocked_by_another_open() {
    use maho_server::server::testing::TestServerHost;
    tokio::time::timeout(std::time::Duration::from_secs(2),async {
        let host=Arc::new(TestServerHost::default());
        host.seed(Some("first".into()),None).await.unwrap();
        host.seed(Some("second".into()),None).await.unwrap();
        let gate=host.gate_next_open_session().await;
        let router=Arc::new(SessionRouter::new(host.clone(),"00000000-0000-4000-8000-000000000001".into()));
        let opening={let router=router.clone();tokio::spawn(async move {router.attach("first").await})};
        gate.entered.wait().await;
        let second=router.attach("second").await.unwrap();
        assert_eq!(host.state.lock().await.open_session_count,2);
        gate.release.resolve(());
        let first=opening.await.unwrap().unwrap();
        first.release().await.unwrap();second.release().await.unwrap();
        router.close().await.unwrap();
    }).await.unwrap();
}
impl ServerHost for Host {
    fn server_services(&self) -> &dyn RoutedServerServiceHost { self }
    fn resolve_session<'a>(&'a self, id: &'a str) -> ServerFuture<'a, Value> {
        Box::pin(async move { Ok(json!({"id":id})) })
    }
    fn open_session(&self, _metadata: Value) -> ServerFuture<'_, Arc<dyn RoutedSessionHandle>> {
        let releases = self.0.clone();
        Box::pin(async move { Ok(Arc::new(Handle(releases)) as Arc<dyn RoutedSessionHandle>) })
    }
}
impl RoutedServerServiceHost for Host {
    fn attach_client(&self, _presentation: Arc<dyn RoutedServerPresentation>) -> ServerFuture<'_, Arc<dyn RoutedServerServiceAttachment>> {
        Box::pin(async { Err(ServerError::new("internal_error", "Unused server services")) })
    }
}
struct Handle(Arc<AtomicUsize>);
impl RoutedSessionHandle for Handle {
    fn attach_client(&self) -> ServerFuture<'_, Arc<dyn RoutedSessionAttachment>> {
        let releases = self.0.clone();
        Box::pin(async move { Ok(Arc::new(Lease(releases)) as Arc<dyn RoutedSessionAttachment>) })
    }
    fn close(&self) -> ServerFuture<'_, ()> { Box::pin(async { Ok(()) }) }
}
struct Lease(Arc<AtomicUsize>);
impl RoutedSessionAttachment for Lease {
    fn invoke_service<'a>(&'a self, call: Value, _publish: Publisher, _context: Context) -> ServerFuture<'a, Option<Value>> {
        Box::pin(async move { Ok(Some(call)) })
    }
    fn release(&self) -> ServerFuture<'_, ()> {
        Box::pin(async move { self.0.fetch_add(1, Ordering::SeqCst); Ok(()) })
    }
}
#[tokio::test]
async fn router_close_waits_for_all_handles_and_repeats_the_same_failure() {
    struct ClosingHost(Arc<tokio::sync::Barrier>);
    struct ClosingHandle(Arc<tokio::sync::Barrier>);
    impl ServerHost for ClosingHost {
        fn server_services(&self)->&dyn RoutedServerServiceHost {self}
        fn resolve_session<'a>(&'a self,id:&'a str)->ServerFuture<'a,Value> {Box::pin(async move {Ok(json!({"id":id}))})}
        fn open_session(&self,_:Value)->ServerFuture<'_,Arc<dyn RoutedSessionHandle>> {Box::pin(async move {Ok(Arc::new(ClosingHandle(self.0.clone())) as Arc<dyn RoutedSessionHandle>)})}
    }
    impl RoutedServerServiceHost for ClosingHost {
        fn attach_client(&self,_:Arc<dyn RoutedServerPresentation>)->ServerFuture<'_,Arc<dyn RoutedServerServiceAttachment>> {Box::pin(async {Err(ServerError::new("internal_error","Unused"))})}
    }
    impl RoutedSessionHandle for ClosingHandle {
        fn attach_client(&self)->ServerFuture<'_,Arc<dyn RoutedSessionAttachment>> {Box::pin(async {Ok(Arc::new(Lease(Arc::new(AtomicUsize::new(0)))) as Arc<dyn RoutedSessionAttachment>)})}
        fn close(&self)->ServerFuture<'_,()> {Box::pin(async move {self.0.wait().await;Err(ServerError::new("internal_error","handle close failed"))})}
    }
    let router=SessionRouter::new(Arc::new(ClosingHost(Arc::new(tokio::sync::Barrier::new(2)))),"00000000-0000-4000-8000-000000000001".into());
    let _first=router.attach("first").await.unwrap();
    let _second=router.attach("second").await.unwrap();
    let (first,second)=tokio::time::timeout(std::time::Duration::from_secs(2),async {tokio::join!(router.close(),router.close())}).await.unwrap();
    assert!(first.is_err());
    assert_eq!(first,second);
    assert_eq!(first,router.close().await);
}

#[tokio::test]
async fn released_attachment_rejects_calls_and_releases_lease_only_once() {
    let releases = Arc::new(AtomicUsize::new(0));
    let router = SessionRouter::new(Arc::new(Host(releases.clone())), "00000000-0000-4000-8000-000000000001".into());
    let attachment = router.attach("session").await.unwrap();
    attachment.release().await.unwrap();
    attachment.release().await.unwrap();
    let (_cancel, cancelled) = tokio::sync::watch::channel(false);
    let result = attachment.invoke(json!({}), Arc::new(|_, _| Box::pin(async { Ok(()) })), Context { cancelled }).await;
    assert_eq!(result.unwrap_err().code, "session_not_attached");
    assert_eq!(releases.load(Ordering::SeqCst), 1);
    router.close().await.unwrap();
}

#[tokio::test]
async fn session_removal_invalidates_all_client_leases() {
    let releases = Arc::new(AtomicUsize::new(0));
    let router = SessionRouter::new(Arc::new(Host(releases.clone())), "00000000-0000-4000-8000-000000000001".into());
    let first = router.attach("session").await.unwrap();
    let second = router.attach("session").await.unwrap();
    router.remove("session").await.unwrap();
    let (_cancel, cancelled) = tokio::sync::watch::channel(false);
    for attachment in [first, second] {
        let result = attachment.invoke(json!({}), Arc::new(|_, _| Box::pin(async { Ok(()) })), Context { cancelled: cancelled.clone() }).await;
        assert_eq!(result.unwrap_err().code, "session_not_attached");
    }
    assert_eq!(releases.load(Ordering::SeqCst), 2);
    router.close().await.unwrap();
}

#[tokio::test]
async fn concurrent_failed_open_shares_one_result_and_later_attach_retries() {
    use maho_server::server::testing::TestServerHost;
    let host=Arc::new(TestServerHost::default());host.seed(None,None).await.unwrap();
    host.state.lock().await.next_open_session_error=Some(ServerError::new("internal_error","faux open failure"));
    let gate=host.gate_next_open_session().await;
    let router=Arc::new(SessionRouter::new(host.clone(),"00000000-0000-4000-8000-000000000001".into()));
    let first={let router=router.clone();tokio::spawn(async move {router.attach("session-1").await})};
    gate.entered.wait().await;
    let second=router.attach("session-1").await.err().expect("second concurrent attach fails");
    gate.release.resolve(());
    let first=first.await.unwrap().err().expect("first concurrent attach fails");
    assert_eq!(first,second);
    assert_eq!(first.message,"faux open failure");
    assert_eq!(host.state.lock().await.open_session_count,1);
    let retried=router.attach("session-1").await.unwrap();
    assert_eq!(host.state.lock().await.open_session_count,2);
    retried.release().await.unwrap();
    router.close().await.unwrap();
}

#[tokio::test]
async fn cancelled_leader_releases_waiter_which_then_opens() {
    use maho_server::server::testing::TestServerHost;
    tokio::time::timeout(std::time::Duration::from_secs(2),async {
        let host=Arc::new(TestServerHost::default());host.seed(None,None).await.unwrap();
        let gate=host.gate_next_open_session().await;
        let router=Arc::new(SessionRouter::new(host.clone(),"00000000-0000-4000-8000-000000000001".into()));
        let leader={let router=router.clone();tokio::spawn(async move {router.attach("session-1").await})};
        gate.entered.wait().await;
        let mut waiter=Box::pin(router.attach("session-1"));
        assert!(futures_util::poll!(waiter.as_mut()).is_pending());
        leader.abort();
        assert!(leader.await.err().expect("leader task was aborted").is_cancelled());
        let attachment=waiter.await.unwrap();
        assert_eq!(host.state.lock().await.open_session_count,2);
        assert_eq!(host.latest_harness("session-1").await.unwrap().state.lock().await.attached_clients,1);
        attachment.release().await.unwrap();
        router.close().await.unwrap();
    }).await.unwrap();
}

#[tokio::test]
async fn client_service_calls_serialize_on_one_attachment() {
    use maho_server::server::testing::TestServerHost;
    let host=Arc::new(TestServerHost::default());host.seed(None,None).await.unwrap();
    let router=SessionRouter::new(host.clone(),"00000000-0000-4000-8000-000000000001".into());
    let attachment=router.attach("session-1").await.unwrap();
    let harness=host.latest_harness("session-1").await.unwrap();
    let gate=harness.gate_next_service_call().await;
    let (_cancel,cancelled)=tokio::sync::watch::channel(false);
    let publish:Publisher=Arc::new(|_,_|Box::pin(async {Ok(())}));
    let first={let attachment=attachment.clone();let context=Context {cancelled:cancelled.clone()};let publish=publish.clone();tokio::spawn(async move {attachment.invoke(json!({"n":1}),publish,context).await})};
    gate.entered.wait().await;
    let mut second=Box::pin(attachment.invoke(json!({"n":2}),publish.clone(),Context {cancelled:cancelled.clone()}));
    assert!(futures_util::FutureExt::now_or_never(second.as_mut()).is_none());
    assert_eq!(harness.state.lock().await.service_calls.len(),1);
    gate.release.resolve(());
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    assert_eq!(harness.state.lock().await.service_calls.len(),2);
    attachment.release().await.unwrap();
    router.close().await.unwrap();
}

#[tokio::test]
async fn terminated_handle_invalidates_hosted_session_and_releases_leases() {
    use maho_server::server::testing::TestServerHost;
    let host=Arc::new(TestServerHost::default());host.seed(None,None).await.unwrap();
    let router=Arc::new(SessionRouter::new(host.clone(),"00000000-0000-4000-8000-000000000001".into()));
    router.bind_self();
    let attachment=router.attach("session-1").await.unwrap();
    let harness=host.latest_harness("session-1").await.unwrap();
    assert_eq!(harness.state.lock().await.attached_clients,1);
    let mut events=router.invalidation_events();
    events.borrow_and_update();
    harness.terminate(ServerError::new("internal_error","session crashed")).await;
    harness.closed.wait().await;
    await_invalidation(&mut events,1).await;
    assert_eq!(harness.state.lock().await.attachment_release_count,1);
    assert_eq!(harness.state.lock().await.attached_clients,0);
    let (_cancel,cancelled)=tokio::sync::watch::channel(false);
    let result=attachment.invoke(json!({}),Arc::new(|_,_|Box::pin(async {Ok(())})),Context {cancelled}).await;
    assert_eq!(result.unwrap_err().code,"session_not_attached");
    router.close().await.unwrap();
}

#[tokio::test]
async fn stale_terminated_handle_cannot_invalidate_reopened_session() {
    use maho_server::server::testing::host::Deferred;
    struct WatchHandle { terminated: Deferred<Option<ServerError>>, closes: Arc<AtomicUsize> }
    struct WatchHost { handles: std::sync::Mutex<Vec<Arc<WatchHandle>>>, opens: AtomicUsize }
    struct WatchLease;
    impl RoutedSessionAttachment for WatchLease {
        fn invoke_service<'a>(&'a self,_:Value,_:Publisher,_:Context)->ServerFuture<'a,Option<Value>> {Box::pin(async {Ok(None)})}
        fn release(&self)->ServerFuture<'_,()> {Box::pin(async {Ok(())})}
    }
    impl RoutedSessionHandle for WatchHandle {
        fn attach_client(&self)->ServerFuture<'_,Arc<dyn RoutedSessionAttachment>> {Box::pin(async {Ok(Arc::new(WatchLease) as Arc<dyn RoutedSessionAttachment>)})}
        fn terminated(&self)->Option<TerminationFuture> {let terminated=self.terminated.clone();Some(Box::pin(async move {terminated.wait().await}))}
        fn close(&self)->ServerFuture<'_,()> {Box::pin(async move {self.closes.fetch_add(1,Ordering::SeqCst);Ok(())})}
    }
    impl RoutedServerServiceHost for WatchHost {
        fn attach_client(&self,_:Arc<dyn RoutedServerPresentation>)->ServerFuture<'_,Arc<dyn RoutedServerServiceAttachment>> {Box::pin(async {Err(ServerError::new("internal_error","Unused"))})}
    }
    impl ServerHost for WatchHost {
        fn server_services(&self)->&dyn RoutedServerServiceHost {self}
        fn resolve_session<'a>(&'a self,_:&'a str)->ServerFuture<'a,Value> {Box::pin(async {Ok(json!({"id":"s"}))})}
        fn open_session(&self,_:Value)->ServerFuture<'_,Arc<dyn RoutedSessionHandle>> {Box::pin(async move {
            self.opens.fetch_add(1,Ordering::SeqCst);
            let handle=Arc::new(WatchHandle {terminated:Deferred::default(),closes:Arc::new(AtomicUsize::new(0))});
            self.handles.lock().unwrap().push(handle.clone());
            Ok(handle as Arc<dyn RoutedSessionHandle>)
        })}
    }
    tokio::time::timeout(std::time::Duration::from_secs(2),async {
        let host=Arc::new(WatchHost {handles:std::sync::Mutex::new(Vec::new()),opens:AtomicUsize::new(0)});
        let router=Arc::new(SessionRouter::new(host.clone(),"00000000-0000-4000-8000-000000000001".into()));
        router.bind_self();
        let _first=router.attach("s").await.unwrap();
        router.remove("s").await.unwrap();
        let _second=router.attach("s").await.unwrap();
        assert_eq!(host.opens.load(Ordering::SeqCst),2);
        assert_eq!(host.handles.lock().unwrap()[0].closes.load(Ordering::SeqCst),1);
        let mut events=router.invalidation_events();
        events.borrow_and_update();
        host.handles.lock().unwrap()[0].terminated.resolve(Some(ServerError::new("internal_error","stale crash")));
        await_invalidation(&mut events,1).await;
        assert_eq!(host.handles.lock().unwrap()[1].closes.load(Ordering::SeqCst),0,"stale termination must not close the replacement");
        let _third=router.attach("s").await.unwrap();
        assert_eq!(host.opens.load(Ordering::SeqCst),2,"stale termination must not invalidate the replacement");
        host.handles.lock().unwrap()[1].terminated.resolve(Some(ServerError::new("internal_error","replacement crash")));
        await_invalidation(&mut events,2).await;
        assert_eq!(host.handles.lock().unwrap()[1].closes.load(Ordering::SeqCst),0,"invalidate does not close the already-terminated handle");
        let _fourth=router.attach("s").await.unwrap();
        assert_eq!(host.opens.load(Ordering::SeqCst),3,"current termination invalidates and reopens");
        router.close().await.unwrap();
    }).await.unwrap();
}

#[tokio::test]
async fn termination_releases_only_the_matched_handle_leases() {
    use maho_server::server::testing::host::Deferred;
    struct TermHandle { terminated: Deferred<Option<ServerError>>, leases: Arc<AtomicUsize> }
    struct TermLease(Arc<AtomicUsize>);
    struct TermHost { handles: std::sync::Mutex<Vec<Arc<TermHandle>>>, opens: AtomicUsize }
    impl RoutedSessionAttachment for TermLease {
        fn invoke_service<'a>(&'a self,_:Value,_:Publisher,_:Context)->ServerFuture<'a,Option<Value>> {Box::pin(async {Ok(Some(json!({"ok":true})))})}
        fn release(&self)->ServerFuture<'_,()> {Box::pin(async move {self.0.fetch_add(1,Ordering::SeqCst);Ok(())})}
    }
    impl RoutedSessionHandle for TermHandle {
        fn attach_client(&self)->ServerFuture<'_,Arc<dyn RoutedSessionAttachment>> {Box::pin(async move {Ok(Arc::new(TermLease(self.leases.clone())) as Arc<dyn RoutedSessionAttachment>)})}
        fn terminated(&self)->Option<TerminationFuture> {let terminated=self.terminated.clone();Some(Box::pin(async move {terminated.wait().await}))}
        fn close(&self)->ServerFuture<'_,()> {Box::pin(async {Ok(())})}
    }
    impl RoutedServerServiceHost for TermHost {
        fn attach_client(&self,_:Arc<dyn RoutedServerPresentation>)->ServerFuture<'_,Arc<dyn RoutedServerServiceAttachment>> {Box::pin(async {Err(ServerError::new("internal_error","Unused"))})}
    }
    impl ServerHost for TermHost {
        fn server_services(&self)->&dyn RoutedServerServiceHost {self}
        fn resolve_session<'a>(&'a self,_:&'a str)->ServerFuture<'a,Value> {Box::pin(async {Ok(json!({"id":"s"}))})}
        fn open_session(&self,_:Value)->ServerFuture<'_,Arc<dyn RoutedSessionHandle>> {Box::pin(async move {
            self.opens.fetch_add(1,Ordering::SeqCst);
            let handle=Arc::new(TermHandle {terminated:Deferred::default(),leases:Arc::new(AtomicUsize::new(0))});
            self.handles.lock().unwrap().push(handle.clone());
            Ok(handle as Arc<dyn RoutedSessionHandle>)
        })}
    }
    tokio::time::timeout(std::time::Duration::from_secs(2),async {
        let host=Arc::new(TermHost {handles:std::sync::Mutex::new(Vec::new()),opens:AtomicUsize::new(0)});
        let router=Arc::new(SessionRouter::new(host.clone(),"00000000-0000-4000-8000-000000000001".into()));
        router.bind_self();
        let first=router.attach("s").await.unwrap();
        let mut events=router.invalidation_events();
        events.borrow_and_update();
        host.handles.lock().unwrap()[0].terminated.resolve(Some(ServerError::new("internal_error","crashed")));
        await_invalidation(&mut events,1).await;
        assert_eq!(host.handles.lock().unwrap()[0].leases.load(Ordering::SeqCst),1,"matched handle lease released once");
        let (_cancel,cancelled)=tokio::sync::watch::channel(false);
        assert_eq!(first.invoke(json!({}),Arc::new(|_,_|Box::pin(async {Ok(())})),Context {cancelled:cancelled.clone()}).await.unwrap_err().code,"session_not_attached");
        let second=router.attach("s").await.unwrap();
        assert_eq!(host.opens.load(Ordering::SeqCst),2);
        assert_eq!(host.handles.lock().unwrap()[1].leases.load(Ordering::SeqCst),0,"replacement lease must not be released by the old termination");
        assert!(second.invoke(json!({}),Arc::new(|_,_|Box::pin(async {Ok(())})),Context {cancelled}).await.is_ok(),"replacement attachment stays live");
        assert_eq!(host.handles.lock().unwrap()[1].leases.load(Ordering::SeqCst),0);
        second.release().await.unwrap();
        router.close().await.unwrap();
    }).await.unwrap();
}
