use crate::kernels::shared::subprocess_run::KernelMessageCallback;
use crate::tool::types::{HostCellContext, HostCellExecutor};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

pub struct HostEntry {
    signal: maho_ext_api::AbortSignal,
    live: Arc<AtomicBool>,
    done: tokio::task::JoinHandle<Value>,
}
impl HostEntry {
    pub fn start(
        cell_id: String,
        host: HostCellExecutor,
        emit: Option<KernelMessageCallback>,
    ) -> Self {
        let signal = maho_ext_api::AbortSignal::default();
        let live = Arc::new(AtomicBool::new(true));
        let active = live.clone();
        let cancellation = signal.clone();
        let owner_signal = signal.clone();
        let done = tokio::spawn(async move {
            let start = tokio::time::Instant::now();
            let emit = Arc::new(move |message: &Value| {
                if active.load(Ordering::SeqCst)
                    && !cancellation.is_aborted()
                    && let Some(emit) = &emit
                {
                    emit(message);
                }
            });
            let mut result = host(HostCellContext {
                signal: signal.clone(),
                emit,
            })
            .await;
            result["type"] = json!("result");
            result["cellId"] = json!(cell_id);
            result["durationMs"] =
                json!(u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX));
            result
        });
        Self {
            signal: owner_signal,
            live,
            done,
        }
    }
    pub async fn result(&mut self) -> Value {
        match (&mut self.done).await {
            Ok(result) => result,
            Err(error) => {
                json!({"ok":false,"error":{"message":format!("host cell failed: {error}")}})
            }
        }
    }
    pub async fn abort(mut self) {
        self.live.store(false, Ordering::SeqCst);
        self.signal.abort();
        if tokio::time::timeout(Duration::from_secs(1), &mut self.done)
            .await
            .is_err()
        {
            self.done.abort();
        }
    }
}
impl Drop for HostEntry {
    fn drop(&mut self) {
        self.live.store(false, Ordering::SeqCst);
        self.signal.abort();
        self.done.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn abort_awaits_owned_work_and_suppresses_late_output() {
        let (started, mut events) = tokio::sync::mpsc::channel(1);
        let (stopped, mut stopped_events) = tokio::sync::mpsc::channel(1);
        let (output, mut frames) = tokio::sync::mpsc::unbounded_channel();
        let entry = HostEntry::start(
            "owned".into(),
            Box::new(move |context| {
                Box::pin(async move {
                    started.send(()).await.expect("started");
                    context.signal.cancelled().await;
                    (context.emit)(&json!({"type":"text","data":"late"}));
                    stopped.send(()).await.expect("stopped");
                    json!({"ok":false})
                })
            }),
            Some(Arc::new(move |frame| {
                output.send(frame.clone()).expect("frame receiver")
            })),
        );
        tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("start deadline")
            .expect("start event");
        tokio::time::timeout(Duration::from_secs(1), entry.abort())
            .await
            .expect("stop deadline");
        assert_eq!(stopped_events.try_recv(), Ok(()));
        assert!(frames.try_recv().is_err());
    }
}
