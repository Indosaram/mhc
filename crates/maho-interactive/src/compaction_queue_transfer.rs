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
