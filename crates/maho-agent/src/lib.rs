//! Port of senpi packages/agent/src (index.ts): the browser-safe agent loop plus the
//! optional proxy stream function. The Node-only harness (harness/) is declared here and
//! filled by todo 15.

pub mod agent;
pub mod agent_loop;
pub mod assistant_terminal_state;
pub mod empty_assistant_recovery;
pub mod harness;
pub mod proxy;
pub mod search;
pub mod stream_fn;
pub mod tool_arguments;
pub mod tool_name_alias;
pub mod types;

pub use assistant_terminal_state::{
    AgentStreamError, EMPTY_TOOL_USE_DEMOTION_DIAGNOSTIC, ProviderRetryWatchdogAbortError,
    TerminalAssistantMessageEvent, create_terminal_failure_assistant_message, demote_tool_use_without_tool_calls,
    is_stream_idle_timeout_error, normalize_terminal_assistant_message, promote_stop_with_pending_tool_calls,
    should_finalize_idle_as_stop, should_terminate_assistant_turn,
};
pub use empty_assistant_recovery::with_empty_assistant_recovery;
pub use search::{EntrySearchHit, SearchQuery, SessionSearchHit, SessionSearchService, SessionSearchTop};
pub use stream_fn::{NO_DEFAULT_STREAM_FN, get_default_stream_fn, set_default_stream_fn};
pub use tool_arguments::{prepare_agent_tool_call_arguments, prepare_tool_arguments};
pub use tool_name_alias::{
    resolve_call_tool, resolve_tool_name_alias, tool_name_correction_notice, with_tool_name_correction,
};
pub use types::*;

// index.ts also re-exports agent.ts, agent-loop.ts, proxy.ts and the harness surface; those are
// wired here as their modules land.
