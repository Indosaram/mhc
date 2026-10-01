use std::sync::Arc;
use maho_ext_api::{ExtensionContext,ExtensionFailure,ExtensionFuture,NotificationType};
use crate::monitor_continuation::ArmedTimer;
pub type GoalTimerDelivery=Arc<dyn Fn(ArmedTimer)->ExtensionFuture<'static,()>+Send+Sync>;
pub type DueGoalDelivery=Arc<dyn Fn(crate::monitor_continuation::DueGoalContinuation)->ExtensionFuture<'static,()>+Send+Sync>;
#[derive(Default)]
pub struct GoalContinuationTimer { worker:Option<tokio::task::JoinHandle<Result<(),ExtensionFailure>>> }
impl GoalContinuationTimer {
    pub async fn sync_monitor(&mut self,monitor:Arc<std::sync::Mutex<crate::monitor_continuation::MonitorAwareGoalContinuation>>,now:Arc<dyn Fn()->f64+Send+Sync>,ctx:ExtensionContext,delivery:DueGoalDelivery)->Result<(),ExtensionFailure> {
        let armed=monitor.lock().map_err(|error|ExtensionFailure::new(error.to_string()))?.armed_timer;
        let Some(armed)=armed else { return self.cancel().await; };
        let reading_ctx=ctx.clone(); let timestamp=now();
        self.arm(armed,timestamp,ctx,Arc::new(move |expected| {
            let monitor=monitor.clone(); let now=now.clone(); let ctx=reading_ctx.clone(); let delivery=delivery.clone();
            Box::pin(async move {
                let idle=ctx.is_idle(); let pending=ctx.has_pending_messages()?;
                let due={ let mut monitor=monitor.lock().map_err(|error|ExtensionFailure::new(error.to_string()))?;
                    if monitor.armed_timer!=Some(expected) { return Ok(()); }
                    monitor.take_due_continuation(now(),idle,pending)
                };
                if let Some(due)=due { delivery(due).await?; }
                Ok(())
            })
        })).await
    }
    pub fn running(&self)->bool { self.worker.as_ref().is_some_and(|worker|!worker.is_finished()) }
    pub async fn cancel(&mut self)->Result<(),ExtensionFailure> {
        if let Some(worker)=self.worker.take() {
            worker.abort();
            match worker.await { Ok(result)=>result?,Err(error) if error.is_cancelled()=>(),Err(error)=>return Err(ExtensionFailure::new(error.to_string())) }
        }
        Ok(())
    }
    pub async fn arm(&mut self,timer:ArmedTimer,now:f64,ctx:ExtensionContext,delivery:GoalTimerDelivery)->Result<(),ExtensionFailure> {
        self.cancel().await?;
        self.worker=Some(tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs_f64((timer.due_at_ms-now).max(0.0)/1000.0)).await;
            if let Err(error)=delivery(timer).await {
                if crate::stale_context::is_stale_extension_context_error(&error)||!ctx.has_ui { return Ok(()); }
                ctx.ui.notify(&format!("Goal continuation delivery failed: {}",error.message),NotificationType::Error);
            }
            Ok(())
        }));
        Ok(())
    }
}
impl Drop for GoalContinuationTimer { fn drop(&mut self) { if let Some(worker)=&self.worker { worker.abort(); } } }
#[cfg(test)] mod tests {
    use super::*;
    #[tokio::test(start_paused=true)] async fn changed_monitor_deadline_invalidates_old_callback() {
        let mut desired=crate::monitor_continuation::MonitorAwareGoalContinuation::default();
        desired.arm_timer(crate::wait_progress::GoalWaitKind::Monitor,1000.0,1000.0,false,0.0);
        let monitor=Arc::new(std::sync::Mutex::new(desired)); let mut timer=GoalContinuationTimer::default();
        timer.sync_monitor(monitor.clone(),Arc::new(||0.0),crate::test_context::context(),Arc::new(|_|Box::pin(async { panic!("superseded desired-state callback fired") }))).await.unwrap();
        monitor.lock().unwrap().armed_timer=None;
        timer.sync_monitor(monitor,Arc::new(||0.0),crate::test_context::context(),Arc::new(|_|Box::pin(async { panic!("canceled monitor delivered") }))).await.unwrap();
        assert!(!timer.running());
    }
    #[tokio::test(start_paused=true)] async fn deadline_dispatches_once_and_replacement_cancels_old_delivery() {
        let mut timer=GoalContinuationTimer::default();
        let armed=ArmedTimer { kind:crate::wait_progress::GoalWaitKind::Monitor,due_at_ms:1000.0,total_ms:1000.0,drain_fire:false };
        timer.arm(armed,0.0,crate::test_context::context(),Arc::new(|_|Box::pin(async { panic!("replaced delivery fired") }))).await.unwrap();
        let (send,mut receive)=tokio::sync::mpsc::unbounded_channel();
        timer.arm(armed,0.0,crate::test_context::context(),Arc::new(move |due| { let send=send.clone(); Box::pin(async move { send.send(due).unwrap(); Ok(()) }) })).await.unwrap();
        assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(2),receive.recv()).await.unwrap().unwrap(),armed);
        timer.cancel().await.unwrap(); assert!(!timer.running()); assert!(receive.recv().await.is_none());
    }
    #[tokio::test] async fn cancelled_delivery_drops_its_signal_without_firing() {
        let mut timer=GoalContinuationTimer::default(); let (send,receive)=tokio::sync::oneshot::channel::<()>();
        let send=Arc::new(std::sync::Mutex::new(Some(send)));
        timer.arm(ArmedTimer { kind:crate::wait_progress::GoalWaitKind::Monitor,due_at_ms:10_000.0,total_ms:10_000.0,drain_fire:false },0.0,crate::test_context::context(),Arc::new(move |_| { let send=send.clone(); Box::pin(async move { send.lock().unwrap().take().unwrap().send(()).unwrap(); Ok(()) }) })).await.unwrap();
        timer.cancel().await.unwrap(); assert!(receive.await.is_err());
    }
}
