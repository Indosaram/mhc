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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleWaitItem {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
}

/// A named wait source: its live count and, when the producer names them, the items it waits on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleWaitSource {
    pub source: String,
    pub active_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<LifecycleWaitItem>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omitted_items: Option<u64>,
}

/// The named slice of a child's report a parent's waits carry (`Pick<ChildLifecycleReport, ...>`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildReportSummary {
    pub task_id: String,
    pub child_session_id: String,
    pub run_epoch: u64,
    pub state: SessionLifecycleState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleWaits {
    pub children: usize,
    /// Named detail can be incomplete even without truncation: custody need not have a named report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_sources: Option<Vec<LifecycleWaitSource>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_reports: Option<Vec<ChildReportSummary>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_reports_omitted: Option<usize>,
    pub background: Vec<BackgroundWait>,
    pub human: Vec<HumanWait>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundWait {
    pub source: String,
    pub active_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<LifecycleWaitItem>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omitted_items: Option<u64>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleStopCause {
    Deadline,
    GoalGuard,
    TodoGuard,
    Aborted,
    NoWake,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleDeadlineKind {
    Inactivity,
    MaxRuntime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LifecycleAbortSource {
    User,
    System,
    Provider,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LifecycleTodoStop {
    Cap,
}

/// Why the last run stopped, for that run only (`run_id`); cleared when the next lifecycle run starts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleStop {
    pub cause: LifecycleStopCause,
    pub run_id: String,
    pub at: String,
    pub epoch: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<LifecycleDeadlineKind>,
    /// The goal guard reason exactly as `goal-continuation-stopped` recorded it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub todo: Option<LifecycleTodoStop>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abort_source: Option<LifecycleAbortSource>,
    /// `Some(false)` only when the deadline-stop entry write failed; distinct from the snapshot's `persisted`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persisted: Option<bool>,
}

/// Known live activity only. A tool represents the oldest outstanding top-level operation, including waits.
/// `since` is when the session observed the start boundary (tool execution start, assistant stream start),
/// not a timestamp carried by the core event, so it approximates the real start; it stays stable while selected.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "lowercase",
    rename_all_fields = "camelCase"
)]
pub enum LifecycleActivity {
    Tool {
        tool_name: String,
        tool_call_id: String,
        since: String,
        outstanding: usize,
    },
    Model {
        since: String,
    },
}

/// Owner-scoped, wire-only activity ordering, including explicit absence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityRevision {
    pub instance_id: String,
    pub revision: u64,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_stop: Option<LifecycleStop>,
    /// In-memory and wire-only; absent outside running or when the activity is unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<LifecycleActivity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity_revision: Option<ActivityRevision>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The child's own state-entry time, not the parent's evaluation time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
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
#[serde(
    tag = "type",
    rename = "session_lifecycle",
    rename_all_fields = "camelCase"
)]
pub enum SessionLifecycleEvent {
    #[serde(rename = "session_lifecycle")]
    SessionLifecycle {
        session_id: String,
        lifecycle: SessionLifecycle,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn named_waits_last_stop_and_activity_use_the_wire_shape() {
        let waits: LifecycleWaits = serde_json::from_value(json!({
            "children": 1,
            "childSources": [{"source":"task","activeCount":1,"items":[{"id":"t1","label":"build","startedAt":"2026-10-01T00:00:00.000Z"}]}],
            "childReports": [{"taskId":"t1","childSessionId":"c","runEpoch":1,"state":"running","label":"build"}],
            "background": [{"source":"terminal","activeCount":2,"omittedItems":3}],
            "human": []
        }))
        .expect("waits");
        assert_eq!(waits.child_sources.as_ref().map(Vec::len), Some(1));
        assert_eq!(waits.background[0].omitted_items, Some(3));
        let stop = LifecycleStop {
            cause: LifecycleStopCause::Deadline,
            run_id: "r".into(),
            at: "now".into(),
            epoch: 2,
            deadline: Some(LifecycleDeadlineKind::MaxRuntime),
            guard: None,
            todo: None,
            abort_source: None,
            persisted: Some(false),
        };
        assert_eq!(
            serde_json::to_value(&stop).expect("stop"),
            json!({"cause":"deadline","runId":"r","at":"now","epoch":2,"deadline":"max_runtime","persisted":false})
        );
        let activity = LifecycleActivity::Tool {
            tool_name: "bash".into(),
            tool_call_id: "c1".into(),
            since: "now".into(),
            outstanding: 2,
        };
        assert_eq!(
            serde_json::to_value(&activity).expect("activity"),
            json!({"kind":"tool","toolName":"bash","toolCallId":"c1","since":"now","outstanding":2})
        );
        assert_eq!(
            serde_json::to_value(LifecycleActivity::Model {
                since: "now".into()
            })
            .expect("model"),
            json!({"kind":"model","since":"now"})
        );
    }
}
