use std::{collections::HashMap, future::Future, pin::Pin, sync::{Arc, Mutex}, task::{Context, Poll}};

use maho_ai::utils::abort::{AbortController, AbortReason, AbortSignal};
use maho_ext_api::{EvalHandleError, EvalHandleKind, EvalHandleOutcome, EvalHandlePhase, EvalHandleRef, EvalHandleSnapshot, EvalHandleUpdateStream, EvalHandleWatch};
use serde_json::Value;

struct Entry { snapshot: EvalHandleSnapshot, outcome: Option<EvalHandleOutcome>, updates: Vec<(String,Arc<WatchSubscription>)>, controller: AbortController, task: Option<tokio::task::AbortHandle>, timer: Option<tokio::task::JoinHandle<()>> }

struct WatchSubscription { sender: Mutex<Option<tokio::sync::mpsc::UnboundedSender<EvalHandleSnapshot>>>, closed: std::sync::atomic::AtomicBool, waker: Mutex<Option<std::task::Waker>> }
impl WatchSubscription {
    fn push(&self,snapshot:EvalHandleSnapshot){if !self.closed.load(std::sync::atomic::Ordering::Acquire)&&let Some(sender)=self.sender.lock().expect("completion watch sender").as_ref(){let _=sender.send(snapshot);}}
    fn close(&self){if !self.closed.swap(true,std::sync::atomic::Ordering::AcqRel){self.sender.lock().expect("completion watch sender").take();if let Some(waker)=self.waker.lock().expect("completion watch waker").take(){waker.wake();}}}
}

#[derive(Default)]
pub struct CompletionHandleRegistry { entries: Mutex<HashMap<String, Entry>>, disposed: Mutex<bool> }

impl CompletionHandleRegistry {
    pub fn start<F>(self: &Arc<Self>, deadline_ms: f64, run: F) -> Result<EvalHandleRef, EvalHandleError>
    where F: FnOnce(AbortSignal) -> FutureBox + Send + 'static {
        let disposed = self.disposed.lock().expect("completion registry state");
        if *disposed { return Err(stale_error()); }
        let reference = EvalHandleRef { kind: EvalHandleKind::Completion, id: format!("cp_{}", uuid::Uuid::new_v4().simple()), run_epoch: 0 };
        let snapshot = EvalHandleSnapshot { reference: reference.clone(), phase: EvalHandlePhase::Pending, host_status: "running".into(), revision: 1 };
        let controller=AbortController::new();let signal=controller.signal();
        self.entries.lock().expect("completion registry entries").insert(reference.id.clone(), Entry { snapshot, outcome: None, updates: Vec::new(), controller:controller.clone(),task:None,timer:None });
        let timer_registry=Arc::downgrade(self);let timer_reference=reference.clone();let timer_controller=controller.clone();
        let remaining=(deadline_ms-now_ms()).max(0.0) as u64;
        let timer=tokio::spawn(async move {tokio::time::sleep(std::time::Duration::from_millis(remaining)).await;timer_controller.abort(Some(AbortReason::new("TimeoutError","completion handle exceeded its cell's hard deadline")));if let Some(registry)=timer_registry.upgrade(){registry.fail_deadline(&timer_reference);}});
        if let Some(entry)=self.entries.lock().expect("completion registry entries").get_mut(&reference.id){if entry.snapshot.phase==EvalHandlePhase::Pending {entry.timer=Some(timer);} else {timer.abort();}}
        let registry = self.clone();
        let settled_reference = reference.clone();
        let task=tokio::spawn(async move { let result = run(signal).await; registry.settle(&settled_reference, result); });
        if let Some(entry)=self.entries.lock().expect("completion registry entries").get_mut(&reference.id){if entry.snapshot.phase==EvalHandlePhase::Pending {entry.task=Some(task.abort_handle());} else {task.abort();}}
        drop(disposed);
        Ok(reference)
    }

    pub fn watch(self: &Arc<Self>, references: &[EvalHandleRef]) -> Result<EvalHandleWatch, EvalHandleError> {
        let disposed=self.disposed.lock().expect("completion registry state");
        if *disposed {return Err(stale_error());}
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let watch_id=uuid::Uuid::new_v4().to_string();
        let subscription=Arc::new(WatchSubscription{sender:Mutex::new(Some(sender)),closed:std::sync::atomic::AtomicBool::new(false),waker:Mutex::new(None)});
        let mut entries = self.entries.lock().expect("completion registry entries");
        let mut initial = Vec::with_capacity(references.len());
        for reference in references {
            let entry = find_entry(&mut entries, reference)?;
            entry.updates.push((watch_id.clone(),subscription.clone()));
            initial.push(entry.snapshot.clone());
        }
        drop(entries);
        drop(disposed);
        let registry=Arc::downgrade(self);let close_subscription=subscription.clone();
        Ok(EvalHandleWatch { initial, updates: Box::pin(WatchStream{receiver,subscription}), close: Arc::new(move || {
            if let Some(registry)=registry.upgrade(){for entry in registry.entries.lock().expect("completion registry entries").values_mut(){entry.updates.retain(|(id,_)|id!=&watch_id);}}
            close_subscription.close();
        }) })
    }

    pub fn result(&self, reference: &EvalHandleRef) -> Result<EvalHandleOutcome, EvalHandleError> {
        self.assert_live()?;
        let mut entries = self.entries.lock().expect("completion registry entries");
        let entry = find_entry(&mut entries, reference)?;
        entry.outcome.clone().ok_or_else(|| EvalHandleError { code: "eval_handle_pending".into(), message: format!("{} is still running", reference.id), details: None })
    }

    pub fn output_text(&self, reference: &EvalHandleRef) -> Result<String, EvalHandleError> {
        self.assert_live()?;
        let mut entries=self.entries.lock().expect("completion registry entries");
        let entry=find_entry(&mut entries,reference)?;
        Ok(match &entry.outcome { Some(EvalHandleOutcome::Fulfilled {value,..})=>value.as_str().map(str::to_owned).unwrap_or_else(||value.to_string()),Some(EvalHandleOutcome::Rejected {error,..})=>error.message.clone(),None=>String::new() })
    }

    pub fn cancel(&self,reference:&EvalHandleRef)->Result<maho_ext_api::EvalHandleCancelReceipt,EvalHandleError>{
        self.assert_live()?;let mut entries=self.entries.lock().expect("completion registry entries");let entry=find_entry(&mut entries,reference)?;
        let cancelled=entry.snapshot.phase==EvalHandlePhase::Pending;
        if cancelled{
            if let Some(timer)=entry.timer.take(){timer.abort();}
            entry.controller.abort(Some(AbortReason::new("AbortError",format!("{} was cancelled",reference.id))));
            if let Some(task)=entry.task.take(){task.abort();}
            entry.snapshot.phase=EvalHandlePhase::Cancelled;entry.snapshot.host_status="cancelled".into();entry.snapshot.revision+=1;
            let error=EvalHandleError{code:"eval_handle_cancelled".into(),message:format!("{} was cancelled",reference.id),details:None};
            entry.outcome=Some(EvalHandleOutcome::Rejected{reference:reference.clone(),error});
            for(_,update)in &entry.updates{update.push(entry.snapshot.clone());}
        }
        Ok(maho_ext_api::EvalHandleCancelReceipt{reference:reference.clone(),cancelled,phase:entry.snapshot.phase})
    }

    pub fn dispose(&self) {
        let mut disposed = self.disposed.lock().expect("completion registry state");
        if *disposed { return; }
        *disposed = true;
        let mut entries = self.entries.lock().expect("completion registry entries");
        for entry in entries.values_mut() {
            if entry.snapshot.phase == EvalHandlePhase::Pending {
                if let Some(timer)=entry.timer.take(){timer.abort();}
                entry.controller.abort(Some(AbortReason::new("AbortError","session generation dropped")));
                if let Some(task)=entry.task.take(){task.abort();}
                entry.snapshot.phase = EvalHandlePhase::Lost;
                entry.snapshot.host_status = "session-dropped".into();
                entry.snapshot.revision += 1;
                entry.outcome = Some(EvalHandleOutcome::Rejected { reference: entry.snapshot.reference.clone(), error: EvalHandleError { code: "eval_handle_lost".into(), message: "session generation dropped".into(), details: None } });
                for (_,update) in &entry.updates { update.push(entry.snapshot.clone()); }
            }
            for (_,update) in &entry.updates{update.close();}
            entry.updates.clear();
        }
    }

    fn assert_live(&self) -> Result<(), EvalHandleError> {
        if *self.disposed.lock().expect("completion registry state") { Err(stale_error()) } else { Ok(()) }
    }

    fn settle(&self, reference: &EvalHandleRef, result: Result<Value, EvalHandleError>) {
        let mut entries = self.entries.lock().expect("completion registry entries");
        let Some(entry) = entries.get_mut(&reference.id) else { return; };
        if entry.snapshot.phase != EvalHandlePhase::Pending { return; }
        let outcome = match result {
            Ok(value) => { entry.snapshot.phase = EvalHandlePhase::Succeeded; entry.snapshot.host_status = "completed".into(); EvalHandleOutcome::Fulfilled { reference: reference.clone(), value } }
            Err(error) => { entry.snapshot.phase = EvalHandlePhase::Failed; entry.snapshot.host_status = "failed".into(); EvalHandleOutcome::Rejected { reference: reference.clone(), error } }
        };
        entry.snapshot.revision += 1;
        entry.outcome = Some(outcome);
        if let Some(timer)=entry.timer.take(){timer.abort();}
        entry.task.take();
        for (_,update) in &entry.updates { update.push(entry.snapshot.clone()); }
        entry.updates.retain(|(_,update)|!update.closed.load(std::sync::atomic::Ordering::Acquire));
    }

    fn fail_deadline(&self,reference:&EvalHandleRef){
        let mut entries=self.entries.lock().expect("completion registry entries");let Some(entry)=entries.get_mut(&reference.id)else{return;};
        if entry.snapshot.phase!=EvalHandlePhase::Pending{return;}
        entry.controller.abort(Some(AbortReason::new("TimeoutError","completion handle exceeded its cell's hard deadline")));
        if let Some(task)=entry.task.take(){task.abort();}
        entry.snapshot.phase=EvalHandlePhase::Failed;entry.snapshot.host_status="deadline".into();entry.snapshot.revision+=1;
        entry.outcome=Some(EvalHandleOutcome::Rejected{reference:reference.clone(),error:EvalHandleError{code:"completion_deadline".into(),message:"completion handle exceeded its cell's hard deadline".into(),details:None}});
        for(_,update)in &entry.updates{update.push(entry.snapshot.clone());}
    }
}

pub type FutureBox=Pin<Box<dyn Future<Output=Result<Value,EvalHandleError>>+Send>>;
fn now_ms()->f64{std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0,|duration|duration.as_secs_f64()*1000.0)}

fn stale_error() -> EvalHandleError { EvalHandleError { code: "eval_handle_stale".into(), message: "this session generation was dropped; its handles are gone".into(), details: None } }

fn find_entry<'a>(entries: &'a mut HashMap<String, Entry>, reference: &EvalHandleRef) -> Result<&'a mut Entry, EvalHandleError> {
    if reference.kind != EvalHandleKind::Completion { return Err(EvalHandleError { code: "eval_handle_not_found".into(), message: format!("no completion handle {}", reference.id), details: None }); }
    let entry = entries.get_mut(&reference.id).ok_or_else(|| EvalHandleError { code: "eval_handle_not_found".into(), message: format!("no completion handle {}", reference.id), details: None })?;
    if entry.snapshot.reference.run_epoch != reference.run_epoch { return Err(stale_error()); }
    Ok(entry)
}

struct WatchStream { receiver:tokio::sync::mpsc::UnboundedReceiver<EvalHandleSnapshot>,subscription:Arc<WatchSubscription> }
impl EvalHandleUpdateStream for WatchStream {
    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<EvalHandleSnapshot>> {
        *self.subscription.waker.lock().expect("completion watch waker")=Some(context.waker().clone());
        if self.subscription.closed.load(std::sync::atomic::Ordering::Acquire){return Poll::Ready(None);}
        self.receiver.poll_recv(context)
    }
}
