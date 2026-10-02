use std::{collections::BTreeMap,sync::{Arc,Mutex,atomic::{AtomicUsize,Ordering}}};
use maho_omo_task::process_sweep::{SessionStartProcessSweepOptions,SENPI_RPC_CHILD_MARKER_ENV,run_session_start_process_sweep};
mod support;
#[tokio::test] async fn session_start_returns_before_sweep_finishes_and_reports_failure() {
    use maho_ext_api::{EventKind,ExtensionEvent,SessionStartEvent,SessionReason};
    let (started_tx,started_rx)=tokio::sync::oneshot::channel(); let started=Mutex::new(Some(started_tx));
    let (release_tx,release_rx)=std::sync::mpsc::channel(); let release=Mutex::new(release_rx);
    let (logged_tx,logged_rx)=tokio::sync::oneshot::channel(); let logged=Mutex::new(Some(logged_tx));
    let mut api=support::api(); maho_omo_task::process_sweep::wire_session_start_process_sweep(&mut api,SessionStartProcessSweepOptions { env:BTreeMap::new(),sweep:Arc::new(move || { started.lock().expect("started").take().expect("once").send(()).expect("receiver"); release.lock().expect("release").recv().expect("release signal"); Err("failure".into()) }),info:Arc::new(|_| {}),warn:Arc::new(move |_| { logged.lock().expect("logged").take().expect("once").send(()).expect("receiver"); }) });
    let mut event=ExtensionEvent::SessionStart(SessionStartEvent { reason:SessionReason::New,initial_model_provenance:None,previous_session_file:None });
    let result=tokio::time::timeout(std::time::Duration::from_secs(5),api.registered.handlers[&EventKind::SessionStart][0](&mut event,&support::context())).await;
    release_tx.send(()).expect("release worker"); result.expect("nonblocking handler").expect("handler result");
    tokio::time::timeout(std::time::Duration::from_secs(5),started_rx).await.expect("started timeout").expect("started signal"); tokio::time::timeout(std::time::Duration::from_secs(5),logged_rx).await.expect("logged timeout").expect("logged signal");
}
#[test] fn rpc_child_marker_skips_sweep_even_when_empty() { let count=Arc::new(AtomicUsize::new(0)); let calls=count.clone(); let options=SessionStartProcessSweepOptions { env:BTreeMap::from([(SENPI_RPC_CHILD_MARKER_ENV.into(),String::new())]),sweep:Arc::new(move || { calls.fetch_add(1,Ordering::SeqCst); Ok(()) }),info:Arc::new(|_| {}),warn:Arc::new(|_| panic!("unexpected warning")) }; run_session_start_process_sweep(&options); assert_eq!(count.load(Ordering::SeqCst),0); }
#[test] fn parent_session_runs_sweep_once() { let count=Arc::new(AtomicUsize::new(0)); let calls=count.clone(); let options=SessionStartProcessSweepOptions { env:BTreeMap::new(),sweep:Arc::new(move || { calls.fetch_add(1,Ordering::SeqCst); Ok(()) }),info:Arc::new(|_| {}),warn:Arc::new(|_| panic!("unexpected warning")) }; run_session_start_process_sweep(&options); assert_eq!(count.load(Ordering::SeqCst),1); }
#[test] fn failed_sweep_reports_without_propagating() { let errors=Arc::new(Mutex::new(Vec::new())); let logs=errors.clone(); let options=SessionStartProcessSweepOptions { env:BTreeMap::new(),sweep:Arc::new(|| Err("failure".into())),info:Arc::new(|_| {}),warn:Arc::new(move |error| logs.lock().expect("logs").push(error.to_owned())) }; run_session_start_process_sweep(&options); assert_eq!(errors.lock().expect("errors").len(),1); }
