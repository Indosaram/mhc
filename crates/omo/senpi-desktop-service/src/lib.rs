//! Native Rust port of `@oh-my-opencode/senpi-desktop-service/src/service`.
//!
//! Provides child process lifecycle management, JSON-RPC transport client,
//! notification dispatch hub, and desktop service coordination.

pub mod child;
pub mod notifications;
pub mod parse;
pub mod rpc_client;
pub mod service;
pub mod timeouts;

pub use child::{
    acquiring_engine_child_factory, default_engine_path, engine_child_factory,
    AcquiringEngineChildOptions, ChildFactory, EngineChild, EngineSpawnConfig,
};
pub use notifications::{DesktopNotificationError, Listener, NotificationHub, Subscription};
pub use parse::{
    is_audit_event, is_desktop_capabilities, is_stop_path_status, parse_error_data, parse_hello,
    parse_permission, parse_session_opened, HelloHandshake, SessionOpened,
};
pub use rpc_client::{
    CallOptions, DesktopEngineRpcError, DesktopError, DesktopLaunchContextError,
    DesktopServiceError, DesktopServiceErrorCode, RpcClient, RpcClientHooks,
};
pub use service::{DesktopService, DesktopServiceOptions, DesktopSessionOpenParams};
pub use timeouts::{
    CAPABILITIES_TIMEOUT, CAPABILITIES_TIMEOUT_MS, CLOSE_TIMEOUT, CLOSE_TIMEOUT_MS, GRACE_MS,
    GRACE_TIMEOUT, HEARTBEAT_INTERVAL, HEARTBEAT_MS, RESTART_MESSAGE, START_TIMEOUT,
    START_TIMEOUT_MESSAGE, START_TIMEOUT_MS,
};
