//! Wire vocabulary between the server and a session's tool child (senpi#3566).
//!
//! Every frame crosses the private channel as one length-prefixed binary frame (`codec`), never as NDJSON. Payloads
//! above `REF_THRESHOLD` never travel inline: they move as an acknowledged `PayloadRef` (`payload_ref`) in either
//! direction. `trace` is the only lossy frame class; it never carries a job result, a launch proof or a tick.
//!
//! Rust shape: the TS discriminated unions become internally tagged serde enums (`"type"` tag, camelCase fields),
//! so the JSON bytes are identical to upstream. An optional `unknown` field (`Option<Value>`) keeps `null` distinct
//! from absent, like `undefined` vs `null` upstream.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

const MIB: usize = 1024 * 1024;

/// `TOOL_CHILD_LIMITS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolChildLimits {
    /// Largest encoded frame body, in bytes; a larger length header is refused before its body is allocated.
    pub frame_bytes: usize,
    /// Ordinary jobs one child runs at once; nested jobs are bounded by `host_call_capacity()` instead.
    pub jobs: usize,
    /// Wake notifications awaiting `wake_ack`, by count and by summed size; the next one waits (never dropped).
    pub wake_pending: usize,
    /// A job's streamed updates awaiting `update_ack`; the job's next `update()` waits for credit (never dropped).
    pub updates_in_flight: usize,
    pub wake_pending_bytes: usize,
    pub host_call_active: usize,
    pub host_call_waiting: usize,
    /// A payload whose JSON exceeds this many bytes moves by reference, never inline.
    pub ref_threshold: usize,
    /// A ref still unacknowledged after this long is reported overdue (the writer still does not delete it).
    pub ref_ack_ms: u64,
    /// Bytes of ref files one side may own at once.
    pub refs_budget: u64,
    pub trace_per_second: usize,
}

pub const TOOL_CHILD_LIMITS: ToolChildLimits = ToolChildLimits {
    frame_bytes: 16 * MIB,
    jobs: 32,
    wake_pending: 64,
    updates_in_flight: 64,
    wake_pending_bytes: MIB,
    host_call_active: 8,
    host_call_waiting: 256,
    ref_threshold: 4 * MIB,
    ref_ack_ms: 30_000,
    refs_budget: 1024 * MIB as u64,
    trace_per_second: 1024,
};

pub const LAUNCH_CONTAINMENTS: [&str; 4] = ["own-group", "inherited", "job", "breakaway"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LaunchContainment {
    #[serde(rename = "own-group")]
    OwnGroup,
    #[serde(rename = "inherited")]
    Inherited,
    #[serde(rename = "job")]
    Job,
    #[serde(rename = "breakaway")]
    Breakaway,
}

pub const LAUNCH_LIFETIMES: [&str; 2] = ["owned", "detached"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchLifetime {
    Owned,
    Detached,
}

/// Which surface asked for the process.
pub const LAUNCH_ORIGINS: [&str; 6] = [
    "tool",
    "terminal",
    "user_bash",
    "interpreter",
    "monitor",
    "kernel",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchOrigin {
    Tool,
    Terminal,
    UserBash,
    Interpreter,
    Monitor,
    Kernel,
}

/// The launch API the command came through; group semantics differ per API.
pub const LAUNCH_APIS: [&str; 7] = [
    "bun.spawn",
    "bun.spawnSync",
    "bun.shell",
    "cp.spawn",
    "cp.spawnSync",
    "cp.execFileSync",
    "pty.spawn",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LaunchApi {
    #[serde(rename = "bun.spawn")]
    BunSpawn,
    #[serde(rename = "bun.spawnSync")]
    BunSpawnSync,
    #[serde(rename = "bun.shell")]
    BunShell,
    #[serde(rename = "cp.spawn")]
    CpSpawn,
    #[serde(rename = "cp.spawnSync")]
    CpSpawnSync,
    #[serde(rename = "cp.execFileSync")]
    CpExecFileSync,
    #[serde(rename = "pty.spawn")]
    PtySpawn,
}

pub const REF_NACK_REASONS: [&str; 5] = ["cancelled", "corrupt", "missing", "budget", "foreign"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefNackReason {
    Cancelled,
    Corrupt,
    Missing,
    Budget,
    Foreign,
}

impl RefNackReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::Corrupt => "corrupt",
            Self::Missing => "missing",
            Self::Budget => "budget",
            Self::Foreign => "foreign",
        }
    }
}

impl std::fmt::Display for RefNackReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a reverse call reaches on the server (DESIGN PART 1): a tool by `name`, the model, a reserved tool, the tool list.
pub const HOST_CALL_KINDS: [&str; 7] = [
    "tool",
    "completion",
    "reserved",
    "list_tools",
    "authorize",
    "lock_file",
    "unlock_file",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostCallKind {
    Tool,
    Completion,
    Reserved,
    ListTools,
    Authorize,
    LockFile,
    UnlockFile,
}

/// A payload moved through a file in the channel generation's directory. The receiver derives the path itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PayloadRef {
    pub ref_id: String,
    /// The channel's child generation; refs of both directions live under that generation's directory.
    pub generation: u64,
    pub bytes: u64,
    pub sha256: String,
}

/// Deserializes a present JSON value (including `null`) as `Some`; an absent field stays `None` via `default`.
fn present_json<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

/// One parsed transcript entry, with its canonical identity and its exact place in the file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedEntryMeta {
    pub id: String,
    /// Canonical parent from the file line, never from a compacted in-memory mirror.
    pub parent_id: Option<String>,
    pub line_offset: u64,
    pub line_bytes: u64,
    /// Wire bytes minus line bytes after the canonical transform.
    pub line_delta: i64,
    #[serde(
        default,
        deserialize_with = "present_json",
        skip_serializing_if = "Option::is_none"
    )]
    pub entry: Option<Value>,
}

pub type TraceDetail = serde_json::Map<String, Value>;

/// Keys a `trace` detail may never carry: the lossy class never transports results, proofs or ticks.
pub const TRACE_RESERVED_KEYS: [&str; 8] = [
    "value",
    "valueRef",
    "result",
    "ref",
    "args",
    "argsRef",
    "ackTick",
    "invokeTick",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ServerToChild {
    HelloAck {
        generation: u64,
    },
    Job {
        job_id: String,
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_job_id: Option<String>,
        #[serde(
            default,
            deserialize_with = "present_json",
            skip_serializing_if = "Option::is_none"
        )]
        args: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        args_ref: Option<PayloadRef>,
    },
    Cancel {
        job_id: String,
    },
    HostReply {
        call_id: String,
        #[serde(
            default,
            deserialize_with = "present_json",
            skip_serializing_if = "Option::is_none"
        )]
        value: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value_ref: Option<PayloadRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    HostCallRefused {
        call_id: String,
        code: String,
    },
    LaunchAck {
        launch_id: String,
    },
    WakeAck {
        wake_id: String,
    },
    SlotQuery {
        launch_id: String,
    },
    Ping {
        seq: u64,
    },
    /// The server delivered the job's update `seq` (one credit back to the child).
    UpdateAck {
        job_id: String,
        seq: u64,
    },
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ChildToServer {
    /// `job_kinds`: the job kinds this child runs (its manifest); a job of any other kind is refused before it is sent.
    Hello {
        generation: u64,
        pid: i64,
        protocol: u64,
        job_kinds: Vec<String>,
    },
    Heartbeat {
        seq: u64,
    },
    Pong {
        seq: u64,
    },
    CancelAck {
        job_id: String,
    },
    Result {
        job_id: String,
        value: Value,
    },
    ResultRef {
        job_id: String,
        r#ref: PayloadRef,
    },
    JobError {
        job_id: String,
        code: String,
        message: String,
    },
    JobRefused {
        job_id: String,
        code: String,
    },
    HostCall {
        call_id: String,
        job_id: String,
        kind: HostCallKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(
            default,
            deserialize_with = "present_json",
            skip_serializing_if = "Option::is_none"
        )]
        args: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        args_ref: Option<PayloadRef>,
    },
    Launch {
        launch_id: String,
        job_id: String,
        origin: LaunchOrigin,
        api: LaunchApi,
        containment: LaunchContainment,
        lifetime: LaunchLifetime,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pid: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pgid: Option<i64>,
    },
    LaunchProof {
        launch_id: String,
        ack_tick: Option<u64>,
        invoke_tick: u64,
    },
    LaunchDone {
        launch_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signal: Option<String>,
    },
    CommandStarted {
        launch_id: String,
        pid: i64,
        started_at_ms: u64,
        creator_in_job: bool,
        breakaway: bool,
    },
    SlotState {
        launch_id: String,
        ack_tick: Option<u64>,
        invoke_tick: Option<u64>,
        invocations: u64,
    },
    Wake {
        wake_id: String,
        source: String,
        bytes: u64,
        #[serde(
            default,
            deserialize_with = "present_json",
            skip_serializing_if = "Option::is_none"
        )]
        value: Option<Value>,
    },
    Batch {
        job_id: String,
        entries: Vec<ParsedEntryMeta>,
        done: bool,
    },
    /// A running job's partial result, in order (`seq` from 1); large ones by acknowledged reference.
    Update {
        job_id: String,
        seq: u64,
        #[serde(
            default,
            deserialize_with = "present_json",
            skip_serializing_if = "Option::is_none"
        )]
        value: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value_ref: Option<PayloadRef>,
    },
    Trace {
        at_ms: u64,
        event: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<TraceDetail>,
    },
}

/// Either side acknowledges or refuses a ref the other side wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum RefControl {
    RefAck {
        ref_id: String,
    },
    RefNack {
        ref_id: String,
        reason: RefNackReason,
    },
}

impl RefControl {
    pub fn ref_id(&self) -> &str {
        match self {
            Self::RefAck { ref_id } | Self::RefNack { ref_id, .. } => ref_id,
        }
    }
}

/// `ToolChildMessage = ServerToChild | ChildToServer | RefControl`. Serializes as the inner message; deserializes
/// by the `type` tag (`from_value`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ToolChildMessage {
    Server(ServerToChild),
    Child(ChildToServer),
    Ref(RefControl),
}

/// `ToolChildMessageType`: the wire `type` string.
pub type ToolChildMessageType = &'static str;

const SERVER_TO_CHILD_TYPES: [&str; 11] = [
    "hello_ack",
    "job",
    "cancel",
    "host_reply",
    "host_call_refused",
    "launch_ack",
    "wake_ack",
    "slot_query",
    "ping",
    "update_ack",
    "shutdown",
];

const REF_CONTROL_TYPES: [&str; 2] = ["ref_ack", "ref_nack"];

impl ToolChildMessage {
    /// The wire `type` of this message.
    pub fn message_type(&self) -> ToolChildMessageType {
        match self {
            Self::Server(message) => match message {
                ServerToChild::HelloAck { .. } => "hello_ack",
                ServerToChild::Job { .. } => "job",
                ServerToChild::Cancel { .. } => "cancel",
                ServerToChild::HostReply { .. } => "host_reply",
                ServerToChild::HostCallRefused { .. } => "host_call_refused",
                ServerToChild::LaunchAck { .. } => "launch_ack",
                ServerToChild::WakeAck { .. } => "wake_ack",
                ServerToChild::SlotQuery { .. } => "slot_query",
                ServerToChild::Ping { .. } => "ping",
                ServerToChild::UpdateAck { .. } => "update_ack",
                ServerToChild::Shutdown => "shutdown",
            },
            Self::Child(message) => match message {
                ChildToServer::Hello { .. } => "hello",
                ChildToServer::Heartbeat { .. } => "heartbeat",
                ChildToServer::Pong { .. } => "pong",
                ChildToServer::CancelAck { .. } => "cancel_ack",
                ChildToServer::Result { .. } => "result",
                ChildToServer::ResultRef { .. } => "result_ref",
                ChildToServer::JobError { .. } => "job_error",
                ChildToServer::JobRefused { .. } => "job_refused",
                ChildToServer::HostCall { .. } => "host_call",
                ChildToServer::Launch { .. } => "launch",
                ChildToServer::LaunchProof { .. } => "launch_proof",
                ChildToServer::LaunchDone { .. } => "launch_done",
                ChildToServer::CommandStarted { .. } => "command_started",
                ChildToServer::SlotState { .. } => "slot_state",
                ChildToServer::Wake { .. } => "wake",
                ChildToServer::Batch { .. } => "batch",
                ChildToServer::Update { .. } => "update",
                ChildToServer::Trace { .. } => "trace",
            },
            Self::Ref(message) => match message {
                RefControl::RefAck { .. } => "ref_ack",
                RefControl::RefNack { .. } => "ref_nack",
            },
        }
    }

    /// Typed decode of an already guarded frame value, routed by its `type` tag.
    pub fn from_value(value: Value) -> Result<Self, serde_json::Error> {
        let tag = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if SERVER_TO_CHILD_TYPES.contains(&tag) {
            serde_json::from_value(value).map(Self::Server)
        } else if REF_CONTROL_TYPES.contains(&tag) {
            serde_json::from_value(value).map(Self::Ref)
        } else {
            serde_json::from_value(value).map(Self::Child)
        }
    }
}

impl<'de> Deserialize<'de> for ToolChildMessage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Self::from_value(value).map_err(serde::de::Error::custom)
    }
}

impl From<ServerToChild> for ToolChildMessage {
    fn from(message: ServerToChild) -> Self {
        Self::Server(message)
    }
}

impl From<ChildToServer> for ToolChildMessage {
    fn from(message: ChildToServer) -> Self {
        Self::Child(message)
    }
}

impl From<RefControl> for ToolChildMessage {
    fn from(message: RefControl) -> Self {
        Self::Ref(message)
    }
}

/// The only frame class a full budget may drop (counted).
pub const LOSSY_FRAME_TYPES: [ToolChildMessageType; 1] = ["trace"];
