use std::sync::{Arc, Mutex};
use maho_ai::utils::abort::AbortController;

#[derive(Clone, Debug)]
pub struct JobSettlement<R, E> {
    pub result: Option<R>,
    pub error: Option<E>,
}

pub struct SpeculativeJob<S, R, E> {
    pub generation: u64,
    pub snapshot: S,
    pub controller: AbortController,
    pub armed_at_tokens: u64,
    settlement: Arc<Mutex<Option<JobSettlement<R, E>>>>,
    completion: tokio::sync::watch::Receiver<bool>,
}

impl<S, R: Clone, E: Clone> SpeculativeJob<S, R, E> {
    pub fn completed(&self) -> bool { *self.completion.borrow() }

    pub async fn settled(&self) -> JobSettlement<R, E> {
        let mut receiver = self.completion.clone();
        receiver.wait_for(|completed| *completed).await.expect("settlement sender survives until notification");
        self.settlement.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref().expect("settlement precedes completion").clone()
    }
}

pub fn track_speculative_job<S, R: Send + 'static, E: Send + 'static>(
    generation: u64,
    snapshot: S,
    controller: AbortController,
    settled: impl std::future::Future<Output = JobSettlement<R, E>> + Send + 'static,
    armed_at_tokens: u64,
) -> SpeculativeJob<S, R, E> {
    let settlement = Arc::new(Mutex::new(None));
    let output = Arc::clone(&settlement);
    let (sender, completion) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        let value = settled.await;
        *output.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(value);
        sender.send_replace(true);
    });
    SpeculativeJob { generation, snapshot, controller, armed_at_tokens, settlement, completion }
}
