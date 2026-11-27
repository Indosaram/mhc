//! Port of senpi `packages/agent/src/execution/chain-count.ts`.

use serde_json::Value;

/// The refusal reason of a thread call past the user's count limit.
pub const CHAIN_COUNT_LIMIT: &str = "chain_count_limit";

/// A user-started session is at count 0; its first thread call is hop 1.
pub const CHAIN_ORIGIN: u64 = 0;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "This thread call would be hop {count} of its chain; your thread chain count limit is {limit}."
)]
pub struct ChainCountLimitError {
    pub count: u64,
    pub limit: u64,
}

impl ChainCountLimitError {
    pub fn reason(&self) -> &'static str {
        CHAIN_COUNT_LIMIT
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChainCountError {
    #[error("malformed chain count: {0}")]
    Malformed(String),
    #[error(transparent)]
    Limit(#[from] ChainCountLimitError),
}

/// The count a thread call sent from a session at `session_count` carries.
pub fn next_chain_count(session_count: u64) -> u64 {
    session_count + 1
}

/// Parses the count a thread call carries; `None` means the call starts a chain
/// (hop 1 from a user-started session).
pub fn decode_chain_count(value: Option<&Value>) -> Result<u64, ChainCountError> {
    let Some(value) = value else {
        return Ok(next_chain_count(CHAIN_ORIGIN));
    };
    if let Some(n) = value.as_u64()
        && n > CHAIN_ORIGIN
    {
        return Ok(n);
    }
    Err(ChainCountError::Malformed(
        serde_json::to_string(value).unwrap_or_else(|_| "unknown".to_owned()),
    ))
}

/// Admits a thread call that makes its chain `count` hops long. `read_limit` is read at each
/// admission, never cached; `None` admits every call at any length.
pub fn admit_chain_call<F>(count: u64, read_limit: F) -> Result<u64, ChainCountLimitError>
where
    F: FnOnce() -> Option<u64>,
{
    if let Some(limit) = read_limit()
        && count > limit
    {
        return Err(ChainCountLimitError { count, limit });
    }
    Ok(count)
}
