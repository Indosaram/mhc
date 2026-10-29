//! Port of senpi packages/agent/src (index.ts): the browser-safe agent loop plus the
//! optional proxy stream function. The Node-only harness (harness/) is declared here and
//! filled by todo 15.

pub mod agent;
pub mod agent_loop;
pub mod assistant_terminal_state;
pub mod empty_assistant_recovery;
pub mod harness;
pub mod local_failure;
pub mod proxy;
pub mod response_timing;
pub mod search;
pub mod stream_fn;
pub mod tool_arguments;
pub mod tool_name_alias;
pub mod types;

pub use harness::env::nodejs::NodeExecutionEnv;
pub use harness::env::windows_process_tree::{
    WINDOWS_PROCESS_TREE_ROWS, WindowsProcessRow, WindowsTreeKillPlan, WindowsTreeKillRefusal,
    list_windows_process_rows_sync, parse_windows_process_tree_rows, windows_tree_kill_args,
    windows_tree_kill_plan,
};

pub use agent::{
    Agent, AgentContinueError, AgentContinuationOptions, AgentListener, AgentOptions, AgentPromptInput,
    AgentSubscription, PartialAgentState, PrepareNextTurnWithContext,
    PrepareNextTurnWithoutContext, ShouldStopAfterTurnWithSignal, agent_event_sink,
    empty_failure_usage,
};
pub use agent_loop::{
    AgentEventSink, agent_loop, agent_loop_continue, build_provider_context, run_agent_loop,
    run_agent_loop_continue,
};
pub use assistant_terminal_state::{
    AgentStreamError, EMPTY_TOOL_USE_DEMOTION_DIAGNOSTIC, ProviderRetryWatchdogAbortError,
    TerminalAssistantMessageEvent, create_terminal_failure_assistant_message,
    demote_tool_use_without_tool_calls, is_stream_idle_timeout_error,
    normalize_terminal_assistant_message, promote_stop_with_pending_tool_calls,
    should_finalize_idle_as_stop, should_terminate_assistant_turn,
};
pub use empty_assistant_recovery::with_empty_assistant_recovery;
pub use local_failure::{LocalFailure, LocalFailurePort, is_local_failure, local_failure_port};
pub use proxy::{
    ProxyAssistantMessageEvent, ProxySerializableStreamOptions, ProxyStreamOptions, stream_proxy,
};
pub use response_timing::{
    GenerationWindowMessage, ResponseTimingRecorder, STREAMED_DELTA_TYPES,
    create_response_timing_recorder, reasoned_before_first_delta,
};
pub use search::{
    EntrySearchHit, SearchQuery, SessionSearchHit, SessionSearchService, SessionSearchTop,
};
pub use stream_fn::{NO_DEFAULT_STREAM_FN, get_default_stream_fn, set_default_stream_fn};
pub use tool_arguments::{prepare_agent_tool_call_arguments, prepare_tool_arguments};
pub use tool_name_alias::{
    resolve_call_tool, resolve_tool_name_alias, tool_name_correction_notice,
    with_tool_name_correction,
};
pub use types::*;

// index.ts also re-exports the harness surface (`harness/`, `node.ts`); those are declared as the
// `harness` placeholder module and filled by todo 15.
