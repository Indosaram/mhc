//! JSON-RPC stdio framing, dispatch, and parent-watchdog primitives for MCP servers.
//!
//! Rust port of `@oh-my-opencode/mcp-stdio-core`. Consumers typically call
//! [`run_json_rpc_stdio_server`] with a [`JsonRpcStdioServerConfig`].

pub mod record;
pub mod responses;
pub mod server;
pub mod transport;
pub mod types;
pub mod watchdog;

pub use record::is_plain_record;
pub use responses::error_response;
pub use responses::json_rpc_id;
pub use responses::message_from_error;
pub use responses::success_response;
pub use server::DEFAULT_IDLE_TIMEOUT_MS;
pub use server::HandlerErrorObserver;
pub use server::JsonRpcStdioServerConfig;
pub use server::LifecycleHook;
pub use server::ParseErrorResponseOverride;
pub use server::ServerError;
pub use server::ServerOutcome;
pub use server::run_json_rpc_stdio_server;
pub use transport::StdioJsonRpcDecoder;
pub use transport::StdioJsonRpcMessage;
pub use transport::StdioJsonRpcResponseMode;
pub use transport::decode_stdio_json_rpc_messages;
pub use transport::write_stdio_json_rpc_response;
pub use types::JsonRpcError;
pub use types::JsonRpcId;
pub use types::JsonRpcResponse;
pub use types::JsonRpcResult;
pub use types::JsonRpcVersion;
pub use types::McpContent;
pub use types::McpLifecycleLog;
pub use types::McpLogFields;
pub use types::McpToolDescriptor;
pub use types::NoopLog;
pub use watchdog::AliveProbe;
pub use watchdog::DEFAULT_PARENT_POLL_INTERVAL_MS;
pub use watchdog::ParentWatchdogConfig;
pub use watchdog::ParentWatchdogHandle;
pub use watchdog::ProcessLiveness;
pub use watchdog::classify_probe_error;
pub use watchdog::create_parent_watchdog;
pub use watchdog::is_process_alive;
pub use watchdog::parent_process_id;
pub use watchdog::probe_process;
