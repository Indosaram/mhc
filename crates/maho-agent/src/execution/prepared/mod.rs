//! Port of senpi `packages/agent/src/execution/prepared/`.

pub mod executor;
pub mod executor_body;
pub mod executor_freeze;
pub mod executor_pipeline;
pub mod executor_types;
pub mod memory_batch_store;
pub mod pipeline_prepared;
pub mod pipeline_prepared_batch;

pub use executor::create_prepared_executor;
pub use executor_pipeline::{PreparationSource, agent_preparation_pipeline};
pub use executor_types::{
    ConsentClassifier, ConsentReply, ConsentRequest, PreparationPipeline, PrepareReply,
    PrepareRequest, PreparedExecutor, PreparedExecutorOptions, ReleaseReply, ReleaseRequest,
};
pub use memory_batch_store::open_memory_batch_store;
pub use pipeline_prepared::create_prepared_tool_host;
pub use pipeline_prepared_batch::{PreparedBatchRequest, run_prepared_batch};
