//! Port of senpi packages/agent/src/harness/session/jsonl/.

pub mod codec;
pub mod fork;
pub mod io;
pub mod repo;
pub mod storage;
pub mod types;

pub use codec::{JsonlParsedSessionHeader, LegacyV3SessionHeader, parse_jsonl_session_header};
pub use fork::{
    JsonlForkIndex, JsonlForkInput, JsonlForkSourceMetadata, index_fork_input, project_jsonl_fork_write,
    run_jsonl_fork, select_jsonl_fork,
};
pub use io::{
    AppendFn, PublishContentFn, file_value, parse_committed_write, parse_jsonl_transaction, publish_file_atomically,
    publish_jsonl, read_jsonl_header, serialize_jsonl_transaction,
};
pub use repo::{JsonlSessionRepo, JsonlSessionRepoHandle};
pub use storage::JsonlStorage;
pub use types::{
    JSONL_FORMAT_VERSION, JSONL_STORAGE_VERSION, JsonlSessionCreateOptions, JsonlSessionListOptions,
    JsonlSessionMetadata, JsonlSessionRepoOptions, JsonlStorageHeader, JsonlStorageOptions,
};
