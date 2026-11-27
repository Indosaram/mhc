//! Port of senpi packages/agent/src (index.ts): the browser-safe agent loop plus the
//! optional proxy stream function, durable store, execution ledger, prepared pipeline,
//! wake inbox/outbox, transport, conformance suites, and harness.

pub mod agent;
pub mod agent_loop;
pub mod assistant_terminal_state;
pub mod conformance;
pub mod durable;
pub mod egress;
pub mod empty_assistant_recovery;
pub mod execution;
pub mod harness;
pub mod heap_future;
pub mod internal_events;
pub mod invocation_input;
pub mod local_failure;
pub mod proxy;
pub mod proxy_event_reducer;
pub mod public_entry;
pub mod read_folders;
pub mod response_timing;
pub mod search;
pub mod step_commit;
pub mod stream_fn;
pub mod stream_keepalive;
pub mod testing;
pub mod tool_arguments;
pub mod tool_name_alias;
pub mod transport;
pub mod types;

pub use harness::env::nodejs::NodeExecutionEnv;
pub use harness::env::windows_process_tree::{
    WINDOWS_PROCESS_TREE_ROWS, WindowsProcessRow, WindowsTreeKillPlan, WindowsTreeKillRefusal,
    list_windows_process_rows_sync, parse_windows_process_tree_rows, windows_tree_kill_args,
    windows_tree_kill_plan,
};

pub use agent::{
    AgentContinuationOptions, AgentContinueError, AgentListener, AgentOptions, AgentPromptInput,
    AgentSubscription, PartialAgentState, PrepareNextTurnWithContext,
    PrepareNextTurnWithoutContext, ShouldStopAfterTurnWithSignal, agent_event_sink,
    empty_failure_usage,
};
pub use agent_loop::{AgentEventSink, AgentLoopError, build_provider_context};
pub use assistant_terminal_state::{
    AgentStreamError, EMPTY_TOOL_USE_DEMOTION_DIAGNOSTIC, ProviderRetryWatchdogAbortError,
    TerminalAssistantMessageEvent, create_terminal_failure_assistant_message,
    demote_tool_use_without_tool_calls, is_stream_idle_timeout_error,
    normalize_terminal_assistant_message, promote_stop_with_pending_tool_calls, settle_stop_reason,
    should_finalize_idle_as_stop, should_terminate_assistant_turn,
};
pub use durable::{
    AdmissionLine, CommitResult, DurableStore, DurableStoreError, DurableStoreErrorCode,
    DurableStoreOptions, MemoryStoreMedium, OnWritePoint, RecordScan, ScanOrder, SessionHandle,
    SessionHeader, SessionState, SessionStore, SessionStoreError, SessionStoreErrorCode,
    StoreDocument, StoreOperation, StoreRecord, StoreSnapshot, StoreValue, StoreWrite,
    StoreWritePoint, StoredEntry, StoredSession, SyncCommit, SyncCommitStore, SyncSessionHandle,
    SyncSessionStore, append_record_writes, create_admission_line, create_memory_store_medium,
    divergence_write, manifest_write, open_memory_store, open_session_store,
    open_sync_session_store, read_session_state, rewrite_record_write,
};
pub use egress::types::*;
pub use empty_assistant_recovery::with_empty_assistant_recovery;
pub use execution::canonical_digest::canonical_json;
pub use execution::chain_count::{
    CHAIN_COUNT_LIMIT, CHAIN_ORIGIN, ChainCountLimitError, admit_chain_call, decode_chain_count,
    next_chain_count,
};
pub use execution::coordinator::create_batch_coordinator;
pub use execution::coordinator_types::{BatchExecutor, BatchPlan};
pub use execution::device_files::{
    DEVICE_FILE_OPS, DeviceFileArgs, DeviceFileError, DeviceFileErrorCode, DeviceFileOp,
    DeviceFileOptions, DeviceFileOutcome, DeviceFileStat, DeviceFiles, DeviceRunRequest,
    DeviceRunResult, FILE_CHANNEL, FILE_CHANNEL_MAX_BYTES, RUN_CHANNEL, base64_length,
    decode_base64, decode_device_file_args, encode_base64, file_outcome_of, is_local_device_files,
};
pub use execution::device_files_node::local_device_files;
pub use execution::grant_verify::{
    DEFAULT_WORKSPACE, GrantRefusal, GrantRow, GrantSnapshot, GrantVerdict, GrantVerifier,
    GrantVerifierOptions, HAND_CAPABILITY, HandAuthority, WorkspaceRoot, authorize,
    create_grant_verifier,
};
pub use execution::ledger::create_execution_ledger;
pub use execution::loop_hook_host::{
    LoopHookError, LoopHookHost, LoopHookHostOptions, create_loop_hook_host,
};
pub use execution::loop_hook_host_hand::{
    LoopHookHand, LoopHookHandOptions, LoopHookPoint, LoopHookRunner, serve_loop_hooks,
};
pub use execution::plane::{
    NestedRequestFn, PlaneHandler, PlaneMessage, PlaneRefusedError, decode_plane_message,
    encode_plane_message,
};
pub use execution::plane_model::{
    MODEL_PLANE, ModelPlaneRequest, create_model_plane_client, decode_model_plane_request,
    serve_model_plane,
};
pub use execution::plane_session::{
    SESSION_PLANE, SessionPlaneRequest, create_loop_tool_host, create_session_plane_client,
    create_split_tool_host, decode_session_plane_request, loop_side_tool_names,
    serve_session_plane,
};
pub use execution::prepared::executor_freeze::to_store_value;
pub use execution::prepared::executor_types::ConsentClassifier;
pub use execution::prepared::memory_batch_store::open_memory_batch_store;
pub use execution::prepared::pipeline_prepared::create_prepared_tool_host;
pub use execution::remote_device_files::HELLO_WAIT_MS;
pub use execution::remote_epoch::{
    EPOCH_FLOOR_MAX_STEP, FloorRaise, HAND_SELF_SENDER, HandEpochFence, HandEpochFenceOptions,
    create_hand_epoch_fence,
};
pub use execution::remote_executor::{
    HandCall, HandScope, OpenHandScope, RemoteExecutor, RemoteExecutorOptions,
    create_remote_executor,
};
pub use execution::remote_file_call::{file_refusal, file_result};
pub use execution::remote_hand_ledger::{
    HandAdmission, HandLedger, HandLedgerOptions, create_hand_ledger,
};
pub use execution::remote_protocol::{
    EpochRefusal, HAND_CONTROL_LANE, RemoteFile, RemoteHandMessage, RemoteHello, RemoteInvoke,
    RemoteProtocolError, RemoteTerminal, RemoteTerminalReason, RemoteToolResult,
    decode_remote_message, encode_remote_message,
};
pub use execution::remote_tool_host::{
    HandPlacement, RemoteToolHostOptions, create_remote_tool_host,
};
pub use execution::tool_host::{
    BatchStoreMissingError, PreflightCarryMissingError, no_preflight_carry,
};
pub use execution::tool_outcome_events::{
    create_error_tool_result, create_tool_result_message, emit_tool_execution_end,
    emit_tool_execution_update, emit_tool_result_message, should_terminate_tool_batch,
};
pub use execution::wake_delivery::{
    HandWakeDelivery, HandWakeMessage, HandWakeMessageOptions, HandWakeNoticeType, HandWakeOutcome,
    HandWakeSession, HandWakeUserMessageOptions, MalformedHandWakeError, ONE_SHOT_MODES,
    deliver_hand_drop, deliver_hand_wake, parse_hand_wake, scope_session_check,
};
pub use execution::wake_inbox::{
    WakeDeliveryFailure, WakeInbox, WakeInboxOptions, create_wake_inbox,
};
pub use execution::wake_link::{
    BrainWakeLink, BrainWakeLinkOptions, HAND_WAKE_LANE, HandWakeLink, HandWakeLinkOptions,
    WakeDrop, WakeLinkMessage, WakeRefusedError, create_hand_wake_link, decode_wake_link_message,
    encode_wake_link_message, serve_hand_wakes,
};
pub use execution::wake_outbox::{
    MalformedWakeError, WakeOutbox, WakeOutboxOptions, create_wake_outbox,
};
pub use execution::wake_types::{
    WAKE_DROPPED_SOURCE, WAKE_RETENTION_MS, WakeDropReason, WakeEvent, WakeLimits, WakeReceipt,
    WakeRefusal,
};
pub use internal_events::{
    InternalAgentChannel, OrderedAgentRecord, create_internal_agent_channel,
};
pub use local_failure::{LocalFailure, LocalFailurePort, is_local_failure, local_failure_port};
pub use proxy::{
    ProxyAssistantMessageEvent, ProxySerializableStreamOptions, ProxyStreamOptions, stream_proxy,
};
pub use public_entry::{
    Agent, agent_loop, agent_loop_continue, run_agent_loop, run_agent_loop_continue,
};
pub use response_timing::{
    GenerationWindowMessage, ResponseTimingRecorder, STREAMED_DELTA_TYPES,
    create_response_timing_recorder, reasoned_before_first_delta,
};
pub use search::{
    EntrySearchHit, SearchQuery, SessionSearchHit, SessionSearchService, SessionSearchTop,
};
pub use step_commit::{StepCommitError, StepCommitFailure, is_step_commit_error};
pub use stream_fn::{NO_DEFAULT_STREAM_FN, get_default_stream_fn, set_default_stream_fn};
pub use tool_arguments::{prepare_agent_tool_call_arguments, prepare_tool_arguments};
pub use tool_name_alias::{
    resolve_call_tool, resolve_tool_name_alias, tool_name_correction_notice,
    with_tool_name_correction,
};
pub use transport::{
    CarrierLink, InProcessNetwork, LocalEndpoint, ReliableTransportOptions, TransportClock,
    TransportFrame, TransportSide, create_in_process_network, create_reliable_transport,
    local_endpoint,
};
pub use types::*;
