use std::future::Future;
pub use maho_core::agent_session::PromptDisposition;
use maho_ext_api::StreamingBehavior;

#[derive(Debug, Clone)]
pub struct CompactionQueuedMessage {
    pub text: String,
    pub mode: StreamingBehavior,
    pub enqueue_order: Option<u64>,
    pub pending_echo_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TransferOptions { pub will_retry: bool, pub defer_admission: bool }

pub trait TransferDependencies {
    type Error;
    fn take_batch(&mut self) -> Vec<CompactionQueuedMessage>;
    fn commit_accepted(&mut self, message: &CompactionQueuedMessage) -> bool;
    fn restore_undelivered(&mut self, messages: &[CompactionQueuedMessage]) -> usize;
    fn is_command(&self, message: &CompactionQueuedMessage) -> bool;
    fn deliver_command(&mut self, message: &CompactionQueuedMessage) -> impl Future<Output = Result<(), Self::Error>>;
    fn deliver_first_prompt(&mut self, message: &CompactionQueuedMessage) -> impl Future<Output = Result<PromptDisposition, Self::Error>>;
    fn deliver_queued(&mut self, message: &CompactionQueuedMessage) -> impl Future<Output = Result<(), Self::Error>>;
    fn report_failure(&mut self, error: Self::Error, undelivered_count: usize);
}

pub async fn transfer_compaction_queue(dependencies: &mut impl TransferDependencies, options: TransferOptions) {
    let batch = dependencies.take_batch();
    let mut prompt_work_owned = options.will_retry || options.defer_admission;
    for (accepted_count, message) in batch.iter().enumerate() {
        let result = if dependencies.is_command(message) {
            dependencies.deliver_command(message).await
        } else if !prompt_work_owned {
            dependencies.deliver_first_prompt(message).await.map(|disposition| {
                prompt_work_owned = disposition != PromptDisposition::Handled;
            })
        } else {
            dependencies.deliver_queued(message).await
        };
        if let Err(error) = result {
            let restored = dependencies.restore_undelivered(&batch[accepted_count..]);
            if restored > 0 { dependencies.report_failure(error, restored); }
            return;
        }
        if !dependencies.commit_accepted(message) { return; }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PromptAdmissionError<E> {
    #[error("Queued prompt was rejected before acceptance")]
    Rejected,
    #[error("{0}")]
    Prompt(E),
    #[error("Prompt task ended without reporting completion")]
    TaskEnded,
}

#[derive(Clone, Copy, Default)]
struct AdmissionState { accepted: bool, rejected: bool, disposition: Option<PromptDisposition> }

pub async fn wait_for_prompt_disposition<E, F>(
    tasks: &mut tokio::task::JoinSet<()>,
    start_prompt: impl FnOnce(std::sync::Arc<dyn Fn(bool) + Send + Sync>, std::sync::Arc<dyn Fn(PromptDisposition) + Send + Sync>) -> F,
    report_post_acceptance_failure: impl FnOnce(E) + Send + 'static,
) -> Result<PromptDisposition, PromptAdmissionError<E>>
where E: Send + 'static, F: Future<Output = Result<(), E>> + Send + 'static {
    let (state, mut changes) = tokio::sync::watch::channel(AdmissionState::default());
    let preflight = state.clone();
    let disposition = state.clone();
    let prompt = start_prompt(
        std::sync::Arc::new(move |success| { preflight.send_modify(|state| { if success { state.accepted = true; } else { state.rejected = true; } }); }),
        std::sync::Arc::new(move |next| { disposition.send_modify(|state| state.disposition = Some(next)); }),
    );
    let (completed, mut completion) = tokio::sync::oneshot::channel();
    tasks.spawn(async move {
        let result = prompt.await;
        let accepted = state.borrow().accepted;
        match result {
            Err(error) if accepted => { report_post_acceptance_failure(error); drop(completed.send(Ok(()))); }
            result => { drop(completed.send(result)); }
        }
    });
    let mut finished = false;
    loop {
        let current = *changes.borrow_and_update();
        if current.accepted && let Some(disposition) = current.disposition { return Ok(disposition); }
        if finished && current.rejected && !current.accepted { return Err(PromptAdmissionError::Rejected); }
        tokio::select! {
            biased;
            result = &mut completion, if !finished => {
                match result {
                    Ok(Ok(())) => finished = true,
                    Ok(Err(error)) => return Err(PromptAdmissionError::Prompt(error)),
                    Err(_) => return Err(PromptAdmissionError::TaskEnded),
                }
            }
            changed = changes.changed() => {
                if changed.is_err() { return std::future::pending().await; }
            }
        }
    }
}
