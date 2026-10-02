use std::sync::Arc;
use maho_ai::types::{AssistantMessageEventStream, Context, Model, SimpleStreamOptions};

pub type OpenAiResponsesStreamRunner = Arc<dyn Fn(&Model, &Context, &SimpleStreamOptions) -> AssistantMessageEventStream + Send + Sync>;

#[derive(Clone, Default)]
pub struct SpeculativeJobSettlement {
    pub on_speculative_job_settled: Option<Arc<dyn Fn() + Send + Sync>>,
}
