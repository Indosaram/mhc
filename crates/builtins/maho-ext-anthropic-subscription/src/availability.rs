use std::{future::Future, path::Path, pin::Pin, process::Stdio, sync::Arc};
use maho_ai::utils::abort::AbortSignal;
pub const AMBIENT_STATUS_TTL_MS: u64 = 30_000;
pub const AMBIENT_PROBE_TIMEOUT_MS: u64 = 10_000;
pub type AmbientProbe = Arc<dyn Fn() -> Pin<Box<dyn Future<Output=anyhow::Result<bool>>+Send>>+Send+Sync>;
type ProbeReceipt = Result<bool,Arc<anyhow::Error>>;
struct State {cached:Option<(u64,bool)>,in_flight:Option<tokio::sync::watch::Receiver<Option<ProbeReceipt>>>}
pub struct AmbientAuthStatusReader {
    state:tokio::sync::Mutex<State>,probe:AmbientProbe,now:Arc<dyn Fn()->u64+Send+Sync>,ttl_ms:u64,
}
impl AmbientAuthStatusReader {
    pub fn new(probe:AmbientProbe,now:Arc<dyn Fn()->u64+Send+Sync>,ttl_ms:u64)->Arc<Self> {
        Arc::new(Self {state:tokio::sync::Mutex::new(State {cached:None,in_flight:None}),probe,now,ttl_ms})
    }
    pub async fn read(self:&Arc<Self>,signal:Option<&AbortSignal>)->anyhow::Result<bool> {
        if let Some(signal)=signal {signal.throw_if_aborted()?;}
        let mut state=self.state.lock().await;
        if let Some((at,value))=state.cached && (self.now)().saturating_sub(at)<self.ttl_ms {return Ok(value);}
        let mut receipt=if let Some(receipt)=&state.in_flight {receipt.clone()} else {
            let (sender,receiver)=tokio::sync::watch::channel(None);state.in_flight=Some(receiver.clone());
            let reader=self.clone();tokio::spawn(async move {
                let result=(reader.probe)().await.map_err(Arc::new);let mut state=reader.state.lock().await;
                if let Ok(value)=result {state.cached=Some(((reader.now)(),value));}
                state.in_flight=None;sender.send_replace(Some(result));
            });receiver
        };drop(state);
        let wait=async {
            loop {
                if let Some(result)=receipt.borrow().clone() {return result.map_err(|error|anyhow::anyhow!("{error}"));}
                receipt.changed().await.map_err(|_|anyhow::anyhow!("Ambient auth probe closed without result"))?;
            }
        };
        match signal {Some(signal)=>maho_ai::utils::abort::race_with_abort_signal(wait,signal).await?,None=>wait.await}
    }
}
pub async fn probe_ambient_claude_auth_status(executable:&Path,timeout_ms:u64)->bool {
    let Ok(mut child)=tokio::process::Command::new(executable).args(["auth","status"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true).spawn() else {return false;};
    match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms),child.wait()).await {
        Ok(Ok(status))=>status.success(),Ok(Err(_))=>false,
        Err(_)=>{if child.start_kill().is_ok() {let _=child.wait().await;}false},
    }
}
#[cfg(test)]
mod tests {
    use super::*;use std::sync::{Mutex,atomic::{AtomicU64,AtomicUsize,Ordering}};
    #[tokio::test]
    async fn concurrent_readers_share_probe_and_expiry_uses_completion_time() {
        let calls=Arc::new(AtomicUsize::new(0));let now=Arc::new(AtomicU64::new(0));let (started,start)=tokio::sync::oneshot::channel();let (release,gate)=tokio::sync::oneshot::channel();
        let gate=Arc::new(Mutex::new(Some((started,gate))));let probe:AmbientProbe={let calls=calls.clone();Arc::new(move || {calls.fetch_add(1,Ordering::SeqCst);let gate=gate.lock().expect("gate").take();Box::pin(async move {if let Some((started,gate))=gate {started.send(()).expect("start");gate.await.expect("release");}Ok(true)})})};
        let reader=AmbientAuthStatusReader::new(probe,{let now=now.clone();Arc::new(move ||now.load(Ordering::SeqCst))},30);
        let first={let reader=reader.clone();tokio::spawn(async move {reader.read(None).await})};start.await.expect("started");
        let mut second=Box::pin(reader.read(None));assert!(std::future::poll_fn(|cx|std::task::Poll::Ready(second.as_mut().poll(cx).is_pending())).await);
        now.store(100,Ordering::SeqCst);release.send(()).expect("release");assert!(first.await.expect("join").expect("first"));assert!(second.await.expect("second"));
        assert_eq!(calls.load(Ordering::SeqCst),1);now.store(129,Ordering::SeqCst);assert!(reader.read(None).await.expect("cached"));assert_eq!(calls.load(Ordering::SeqCst),1);
        now.store(130,Ordering::SeqCst);assert!(reader.read(None).await.expect("new probe"));assert_eq!(calls.load(Ordering::SeqCst),2);
    }
    #[tokio::test]
    async fn caller_abort_leaves_shared_probe_running_and_failures_are_not_cached() {
        let (started,start)=tokio::sync::oneshot::channel();let (release,gate)=tokio::sync::oneshot::channel();let gate=Arc::new(Mutex::new(Some((started,gate))));
        let reader=AmbientAuthStatusReader::new(Arc::new(move || {let gate=gate.lock().expect("gate").take();Box::pin(async move {if let Some((started,gate))=gate {started.send(()).expect("start");gate.await.expect("release");anyhow::bail!("failed probe");}Ok(true)})}),Arc::new(||0),30);
        let controller=maho_ai::utils::abort::AbortController::new();let signal=controller.signal();let task={let reader=reader.clone();tokio::spawn(async move {reader.read(Some(&signal)).await})};start.await.expect("start");controller.abort(None);assert!(task.await.expect("join").is_err());
        let mut pending=Box::pin(reader.read(None));assert!(std::future::poll_fn(|cx|std::task::Poll::Ready(pending.as_mut().poll(cx).is_pending())).await);release.send(()).expect("release");assert!(pending.await.is_err());assert!(reader.read(None).await.expect("retry"));
    }
}
