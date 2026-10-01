//! Port of senpi packages/ai/src/api/devin-agent/types.ts.

use crate::types::StreamOptions;

/// `DevinAgentOptions`: the options the devin-agent (Cascade) API accepts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DevinAgentOptions {
    /// Devin CLI session token. Prefixed on the wire when it is not already.
    pub api_key: Option<String>,
    /// Cascade conversation id. Threads a multi-turn exchange server-side and seeds deterministic
    /// message ids; a fresh id starts a new transcript.
    pub cascade_id: Option<String>,
}

impl DevinAgentOptions {
    /// Reads the API-specific options off a provider-neutral `StreamOptions`.
    pub fn from_stream_options(options: &StreamOptions) -> Self {
        Self {
            api_key: options.request.api_key.clone(),
            cascade_id: options.extra.get("cascadeId").and_then(|value| value.as_str()).map(str::to_owned),
        }
    }
}
