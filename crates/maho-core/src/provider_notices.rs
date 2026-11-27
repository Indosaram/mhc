//! Port of senpi packages/coding-agent/src/core/provider-notices.ts.

/// Shared by provider diagnostics and transcript renderers; RPC carries the same message in details.
pub const LONG_CONTEXT_FALLBACK_DIAGNOSTIC: &str = "claude_sdk_oauth_long_context_fallback";
pub const LONG_CONTEXT_FALLBACK_NOTICE: &str =
    "Claude refused the 1M context window - this request was retried on the same model at 200k";
