//! Shared engine and RPC lifecycle contracts.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionLifecycleState {
    Running,
    WaitingChildren,
    WaitingBackground,
    WaitingHuman,
    Paused,
    Held,
    Done,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HumanWaitKind {
    Question,
    Approval,
    GoalBlocked,
    GoalPaused,
    Stalled,
    InterruptedRun,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HumanWait {
    pub kind: HumanWaitKind,
    pub origin_session_id: String,
    pub request_id: String,
    pub revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_label: Option<String>,
    pub since: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleWaits {
    pub children: usize,
    pub background: Vec<BackgroundWait>,
    pub human: Vec<HumanWait>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundWait {
    pub source: String,
    pub active_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_at_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleHoldKind {
    SessionHold,
    RunFence,
    InheritedHold,
    Unattended,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleHold {
    pub kind: LifecycleHoldKind,
    pub since: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnedBy {
    pub parent_session_id: String,
    pub root_session_id: String,
    pub task_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EpochOrigin {
    Human,
    Automatic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionRole {
    Root,
    Child,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettleDecision {
    pub decision_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompletionKind {
    Goal,
    Run,
    Agent,
    Reply,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleCompletion {
    pub kind: CompletionKind,
    pub at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureReason {
    ProviderError,
    ToolError,
    TaskError,
    Lost,
    Interrupted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleFailure {
    pub reason: FailureReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_at_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_epoch: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLifecycle {
    pub state: SessionLifecycleState,
    pub seq: u64,
    pub epoch: u64,
    pub epoch_origin: EpochOrigin,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_id: Option<String>,
    pub human_input_revision: u64,
    pub self_settle_eligible: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settle_decision: Option<SettleDecision>,
    pub restoring: bool,
    pub persisted: bool,
    pub run_paused: bool,
    pub since: String,
    pub role: SessionRole,
    pub root_session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting_on: Option<LifecycleWaits>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub holds: Option<Vec<LifecycleHold>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion: Option<LifecycleCompletion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<LifecycleFailure>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChildTerminal {
    Completed,
    Error,
    Lost,
    Interrupted,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildLifecycleReport {
    pub task_id: String,
    pub child_session_id: String,
    pub run_epoch: u64,
    pub generation: u64,
    pub state: SessionLifecycleState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting_on: Option<LifecycleWaits>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal: Option<ChildTerminal>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelfSettleReason {
    NotEligible,
    Held,
    NoEpoch,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelfSettleResult {
    pub recorded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<SelfSettleReason>,
    pub epoch: u64,
    pub human_input_revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ObligationKind {
    Children,
    Background,
}

pub type DeliveryTicketId = String;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObligationCloseReason {
    Consumed,
    Cancelled,
    Passive,
}

pub trait ObligationToken: Sized {
    fn transfer(self, next_key: String, kind: Option<ObligationKind>) -> Self;
    fn ticket(&mut self) -> DeliveryTicketId;
    fn held_as_notice(&mut self, notice_id: String);
    fn close(self, reason: ObligationCloseReason);
}

pub trait WorkObligations {
    type Token: ObligationToken;
    fn open(&mut self, key: String, kind: ObligationKind, detail: Option<String>) -> Self::Token;
}

pub trait LifecycleApi {
    type Obligations: WorkObligations;
    fn obligations(&mut self) -> &mut Self::Obligations;
    fn report_child(&mut self, report: ChildLifecycleReport);
    fn mark_run_completed(&mut self, run_id: String);
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename = "session_lifecycle", rename_all_fields = "camelCase")]
pub enum SessionLifecycleEvent {
    #[serde(rename = "session_lifecycle")]
    SessionLifecycle {
        session_id: String,
        lifecycle: SessionLifecycle,
    },
}
