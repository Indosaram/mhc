//! Deterministic reflection state machine: triggers, reservations, and completion promotion.
//!
//! Spec: `docs/cleanroom/specs/memory-reflection-machine.md`. Every transition is a pure function of
//! the supplied state, so the same inputs always produce the same reservation.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::cursor::{
    REFLECTION_SNAPSHOT_MAX_BYTES, ReflectionSnapshot, ReflectionTranscriptState,
    count_completed_steps,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ReflectionTrigger {
    #[serde(rename = "step-count")]
    StepCount,
    #[serde(rename = "compaction")]
    Compaction,
    #[serde(rename = "manual")]
    Manual,
    #[serde(rename = "dream")]
    Dream,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DreamOrigin {
    #[serde(rename = "manual")]
    Manual,
    #[serde(rename = "idle")]
    Idle,
    #[serde(rename = "shutdown")]
    Shutdown,
    #[serde(rename = "pressure")]
    Pressure,
}

impl DreamOrigin {
    /// Whether the origin comes from the store's own scheduler rather than a person asking.
    pub fn is_automatic(self) -> bool {
        matches!(self, Self::Idle | Self::Shutdown | Self::Pressure)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ReflectionOutcome {
    #[serde(rename = "merged")]
    Merged,
    #[serde(rename = "no_changes")]
    NoChanges,
    #[serde(rename = "parent_dirty")]
    ParentDirty,
    #[serde(rename = "merge_conflict")]
    MergeConflict,
    #[serde(rename = "dirty_uncommitted")]
    DirtyUncommitted,
    #[serde(rename = "failed")]
    Failed,
    #[serde(rename = "timed_out")]
    TimedOut,
}

impl ReflectionOutcome {
    /// Whether the run did not land its work, so the journal must not be advanced.
    pub fn is_failure(self) -> bool {
        matches!(
            self,
            Self::ParentDirty
                | Self::MergeConflict
                | Self::DirtyUncommitted
                | Self::Failed
                | Self::TimedOut
        )
    }
}

impl fmt::Display for ReflectionOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Merged => "merged",
            Self::NoChanges => "no_changes",
            Self::ParentDirty => "parent_dirty",
            Self::MergeConflict => "merge_conflict",
            Self::DirtyUncommitted => "dirty_uncommitted",
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
        };
        f.write_str(name)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_compaction: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_max_bytes: Option<u64>,
}

pub const REFLECTION_PENDING_MAX_CONVERSATIONS: usize = 32;
pub const REFLECTION_PENDING_MAX_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalSnapshot {
    pub conversation_id: String,
    pub state: ReflectionTranscriptState,
    pub snapshot: Option<ReflectionSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturedConversation {
    #[serde(rename = "conversationId")]
    pub conversation_id: String,
    pub snapshot: ReflectionSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReflectionRequest {
    pub trigger: ReflectionTrigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<DreamOrigin>,
    #[serde(rename = "conversationIds")]
    pub conversation_ids: Vec<String>,
    pub snapshots: Vec<CapturedConversation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<String>,
    #[serde(rename = "recentN", default, skip_serializing_if = "Option::is_none")]
    pub recent_n: Option<usize>,
    #[serde(rename = "targetDoc", default, skip_serializing_if = "Option::is_none")]
    pub target_doc: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReservedRun {
    #[serde(rename = "runId")]
    pub run_id: String,
    pub request: ReflectionRequest,
    #[serde(
        rename = "reservedAt",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub reserved_at: Option<String>,
    #[serde(
        rename = "launcherPid",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub launcher_pid: Option<u32>,
    #[serde(
        rename = "launcherHostname",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub launcher_hostname: Option<String>,
    #[serde(
        rename = "launcherProcessStart",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub launcher_process_start: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReservationState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<ReservedRun>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<ReservedRun>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineState {
    pub journal: JournalSnapshot,
    pub reservation: ReservationState,
    pub config: TriggerConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReflectionEvent {
    Settled {
        success: bool,
    },
    CompactionAccepted,
    Manual {
        focus: Option<String>,
        recent_n: Option<usize>,
        conversation_ids: Option<Vec<String>>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvaluationAction {
    None,
    Reserve(ReflectionRequest),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluationResult {
    pub state: MachineState,
    pub action: EvaluationAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompleteTransition {
    pub state: ReservationState,
    pub finalize: Vec<CapturedConversation>,
    pub clear_pending_compaction: Vec<String>,
    pub launch: Option<ReservedRun>,
}

/// A settled turn reserves work in priority order: compaction first, then the step threshold, then
/// pressure. Each candidate is suppressed while a run of that trigger is already active or pending.
pub fn evaluate_transitions(mut state: MachineState, event: ReflectionEvent) -> EvaluationResult {
    match event {
        ReflectionEvent::CompactionAccepted => {
            state.journal.state.pending_compaction = Some(true);
            EvaluationResult {
                state,
                action: EvaluationAction::None,
            }
        }
        ReflectionEvent::Manual {
            focus,
            recent_n,
            conversation_ids,
        } => {
            let ids = match conversation_ids {
                Some(ref list) if !list.is_empty() => list.clone(),
                _ => vec![state.journal.conversation_id.clone()],
            };
            let request = make_request(
                &state.journal,
                ReflectionTrigger::Manual,
                ids,
                focus,
                recent_n,
            );
            EvaluationResult {
                state,
                action: EvaluationAction::Reserve(request),
            }
        }
        ReflectionEvent::Settled { success } => {
            if !success {
                return EvaluationResult {
                    state,
                    action: EvaluationAction::None,
                };
            }

            let compaction_ready = state.config.on_compaction == Some(true)
                && state.journal.state.pending_compaction == Some(true)
                && !contains_trigger(&state.reservation, ReflectionTrigger::Compaction);

            if compaction_ready {
                let request = make_request(
                    &state.journal,
                    ReflectionTrigger::Compaction,
                    vec![state.journal.conversation_id.clone()],
                    None,
                    None,
                );
                return EvaluationResult {
                    state,
                    action: EvaluationAction::Reserve(request),
                };
            }

            let threshold = state.config.step_count.unwrap_or(0);
            let threshold_ready = threshold > 0
                && state.journal.state.steps_since_last_successful_reflection >= threshold
                && !contains_trigger(&state.reservation, ReflectionTrigger::StepCount);

            if threshold_ready {
                let request = make_request(
                    &state.journal,
                    ReflectionTrigger::StepCount,
                    vec![state.journal.conversation_id.clone()],
                    None,
                    None,
                );
                return EvaluationResult {
                    state,
                    action: EvaluationAction::Reserve(request),
                };
            }
            let budget = state
                .config
                .snapshot_max_bytes
                .unwrap_or(REFLECTION_SNAPSHOT_MAX_BYTES as u64);
            let pressure_ready = budget > 0
                && state
                    .journal
                    .state
                    .unreflected_bytes
                    .is_some_and(|bytes| bytes as u64 >= budget)
                && !contains_trigger(&state.reservation, ReflectionTrigger::StepCount);
            if pressure_ready {
                let mut request = make_request(
                    &state.journal,
                    ReflectionTrigger::Dream,
                    vec![state.journal.conversation_id.clone()],
                    None,
                    None,
                );
                request.origin = Some(DreamOrigin::Pressure);
                return EvaluationResult {
                    state,
                    action: EvaluationAction::Reserve(request),
                };
            }
            EvaluationResult {
                state,
                action: EvaluationAction::None,
            }
        }
    }
}

/// The first request becomes active; any later one is merged into pending and capped.
pub fn reserve_transition(
    mut state: ReservationState,
    request: ReflectionRequest,
    run_id: String,
) -> (ReservationState, &'static str) {
    if state.active.is_none() {
        state.active = Some(ReservedRun {
            run_id,
            request,
            reserved_at: None,
            launcher_pid: None,
            launcher_hostname: None,
            launcher_process_start: None,
        });
        return (state, "active");
    }

    let pending = match state.pending {
        Some(existing) => ReservedRun {
            run_id: existing.run_id,
            request: merge_requests(existing.request, request),
            reserved_at: existing.reserved_at,
            launcher_pid: existing.launcher_pid,
            launcher_hostname: existing.launcher_hostname,
            launcher_process_start: existing.launcher_process_start,
        },
        None => ReservedRun {
            run_id,
            request,
            reserved_at: None,
            launcher_pid: None,
            launcher_hostname: None,
            launcher_process_start: None,
        },
    };

    state.pending = Some(cap_pending_run(pending));
    (state, "pending")
}

/// Trim oldest conversations until both the count and the serialized byte cap hold. A snapshot
/// whose conversation was trimmed is dropped with it, so the two lists never disagree.
fn cap_pending_run(mut run: ReservedRun) -> ReservedRun {
    let mut seen = BTreeSet::new();
    run.request
        .conversation_ids
        .retain(|id| seen.insert(id.clone()));
    run.request
        .snapshots
        .retain(|captured| seen.contains(&captured.conversation_id));
    while seen.len() > REFLECTION_PENDING_MAX_CONVERSATIONS
        || serde_json::to_vec(&run).map_or(0, |bytes| bytes.len()) > REFLECTION_PENDING_MAX_BYTES
    {
        let Some(oldest) = run.request.conversation_ids.first().cloned() else {
            break;
        };
        seen.remove(&oldest);
        run.request.conversation_ids.remove(0);
        run.request
            .snapshots
            .retain(|captured| captured.conversation_id != oldest);
    }
    run
}

/// Only `Merged` and `NoChanges` advance the reflected cursor; every other outcome leaves the
/// journals untouched so the work is retried. A pending run is launched only while its trigger is
/// still satisfied by the updated journals.
pub fn complete_transition(
    mut state: ReservationState,
    run_id: &str,
    outcome: ReflectionOutcome,
    journals: &BTreeMap<String, JournalSnapshot>,
    config: &TriggerConfig,
) -> Result<CompleteTransition, String> {
    if state.active.as_ref().map(|r| r.run_id.as_str()) != Some(run_id) {
        return Err(format!("Reflection run is not active: {run_id}"));
    }

    let Some(active_run) = state.active.take() else {
        return Err(format!("Reflection run is not active: {run_id}"));
    };
    let succeeded = outcome == ReflectionOutcome::Merged || outcome == ReflectionOutcome::NoChanges;
    let finalize = if succeeded {
        active_run.request.snapshots.clone()
    } else {
        Vec::new()
    };

    let clear_pending_compaction = if succeeded
        && active_run.request.trigger == ReflectionTrigger::Compaction
    {
        active_run
            .request
            .conversation_ids
            .iter()
            .filter(|id| journals.get(*id).and_then(|j| j.state.pending_compaction) == Some(true))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };

    let mut effective_journals = journals.clone();
    for captured in &finalize {
        if let Some(current) = effective_journals.get_mut(&captured.conversation_id) {
            let completed = count_completed_steps(&captured.snapshot.entries);
            let reflected = current
                .state
                .total_completed_steps
                .min(current.state.reflected_completed_steps + completed);
            current.state.reflected_completed_steps = reflected;
            current.state.steps_since_last_successful_reflection = current
                .state
                .total_completed_steps
                .saturating_sub(reflected);
        }
    }

    for id in &clear_pending_compaction {
        if let Some(current) = effective_journals.get_mut(id) {
            current.state.pending_compaction = Some(false);
        }
    }

    let pending = state.pending.take();
    let (next_state, launch) = match pending {
        Some(p) if is_still_triggered(&p.request, &effective_journals, config) => {
            let launch_run = p.clone();
            (
                ReservationState {
                    active: Some(p),
                    pending: None,
                },
                Some(launch_run),
            )
        }
        _ => (
            ReservationState {
                active: None,
                pending: None,
            },
            None,
        ),
    };

    Ok(CompleteTransition {
        state: next_state,
        finalize,
        clear_pending_compaction,
        launch,
    })
}

fn make_request(
    journal: &JournalSnapshot,
    trigger: ReflectionTrigger,
    conversation_ids: Vec<String>,
    focus: Option<String>,
    recent_n: Option<usize>,
) -> ReflectionRequest {
    let snapshots = match &journal.snapshot {
        Some(s) => vec![CapturedConversation {
            conversation_id: journal.conversation_id.clone(),
            snapshot: s.clone(),
        }],
        None => Vec::new(),
    };

    ReflectionRequest {
        trigger,
        origin: None,
        conversation_ids: unique_strings(conversation_ids),
        snapshots,
        focus,
        recent_n,
        target_doc: None,
    }
}

fn contains_trigger(state: &ReservationState, trigger: ReflectionTrigger) -> bool {
    state.active.as_ref().map(|r| r.request.trigger) == Some(trigger)
        || state.pending.as_ref().map(|r| r.request.trigger) == Some(trigger)
}

fn trigger_priority(request: &ReflectionRequest) -> f64 {
    match request.trigger {
        ReflectionTrigger::StepCount => 1.0,
        ReflectionTrigger::Compaction => 2.0,
        ReflectionTrigger::Manual => 3.0,
        ReflectionTrigger::Dream => match request.origin.unwrap_or(DreamOrigin::Manual) {
            DreamOrigin::Idle => 1.5,
            DreamOrigin::Pressure => 1.5,
            DreamOrigin::Shutdown => 2.5,
            DreamOrigin::Manual => 3.0,
        },
    }
}

/// The stronger trigger supplies the trigger, origin, focus and target; conversation IDs and
/// snapshots are unioned, and the larger `recent_n` wins.
fn merge_requests(left: ReflectionRequest, right: ReflectionRequest) -> ReflectionRequest {
    let right_prio = trigger_priority(&right);
    let left_prio = trigger_priority(&left);
    let strongest = if right_prio >= left_prio {
        &right
    } else {
        &left
    };

    let mut all_ids = left.conversation_ids.clone();
    all_ids.extend(right.conversation_ids.iter().cloned());
    let conversation_ids = unique_strings(all_ids);

    let snapshots = merge_snapshots(left.snapshots.clone(), right.snapshots.clone());
    let focus = strongest.focus.clone();
    let recent_n = match (left.recent_n, right.recent_n) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    };
    let target_doc = strongest.target_doc.clone();
    let trigger = strongest.trigger;
    let origin = if trigger == ReflectionTrigger::Dream {
        strongest.origin
    } else {
        None
    };

    ReflectionRequest {
        trigger,
        origin,
        conversation_ids,
        snapshots,
        focus,
        recent_n,
        target_doc,
    }
}

/// Right wins for the same conversation: the later capture is the fresher one.
fn merge_snapshots(
    left: Vec<CapturedConversation>,
    right: Vec<CapturedConversation>,
) -> Vec<CapturedConversation> {
    let mut map: BTreeMap<String, CapturedConversation> = BTreeMap::new();
    for item in left {
        map.insert(item.conversation_id.clone(), item);
    }
    for item in right {
        map.insert(item.conversation_id.clone(), item);
    }
    map.into_values().collect()
}

fn unique_strings(values: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for v in values {
        if seen.insert(v.clone()) {
            out.push(v);
        }
    }
    out
}

fn is_still_triggered(
    request: &ReflectionRequest,
    journals: &BTreeMap<String, JournalSnapshot>,
    config: &TriggerConfig,
) -> bool {
    match request.trigger {
        ReflectionTrigger::Manual | ReflectionTrigger::Dream => true,
        ReflectionTrigger::Compaction => {
            config.on_compaction == Some(true)
                && request.conversation_ids.iter().any(|id| {
                    journals.get(id).and_then(|j| j.state.pending_compaction) == Some(true)
                })
        }
        ReflectionTrigger::StepCount => {
            let threshold = config.step_count.unwrap_or(0);
            threshold > 0
                && request.conversation_ids.iter().any(|id| {
                    journals
                        .get(id)
                        .map(|j| j.state.steps_since_last_successful_reflection >= threshold)
                        .unwrap_or(false)
                })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor::TranscriptEntry;

    fn transcript_state() -> ReflectionTranscriptState {
        ReflectionTranscriptState {
            schema_version: "1".to_string(),
            reflected_through_message_id: None,
            reflected_through_byte_offset: None,
            unreflected_bytes: None,
            total_completed_steps: 0,
            reflected_completed_steps: 0,
            steps_since_last_successful_reflection: 0,
            last_reflection_started_at: None,
            last_reflection_succeeded_at: None,
            pending_compaction: None,
            consecutive_failures: None,
            next_eligible_at: None,
        }
    }

    fn snapshot(entries: usize) -> ReflectionSnapshot {
        ReflectionSnapshot {
            start_message_id: "m1".to_string(),
            end_message_id: "m2".to_string(),
            start_line: 0,
            end_snapshot_line: entries,
            entries: (0..entries)
                .map(|i| TranscriptEntry {
                    kind: "assistant".to_string(),
                    text: Some(format!("step {i}")),
                    captured_at: format!("2026-01-01T00:00:0{i}Z"),
                    source_line_id: format!("line-{i}"),
                    source_message_id: format!("msg-{i}"),
                })
                .collect(),
            backlog_remaining: None,
        }
    }

    fn journal(id: &str, steps: usize, pending_compaction: bool) -> JournalSnapshot {
        let mut state = transcript_state();
        state.steps_since_last_successful_reflection = steps;
        state.total_completed_steps = steps;
        state.pending_compaction = Some(pending_compaction);
        JournalSnapshot {
            conversation_id: id.to_string(),
            state,
            snapshot: Some(snapshot(1)),
        }
    }

    fn machine(journal: JournalSnapshot, config: TriggerConfig) -> MachineState {
        MachineState {
            journal,
            reservation: ReservationState::default(),
            config,
        }
    }

    fn threshold(steps: usize) -> TriggerConfig {
        TriggerConfig {
            step_count: Some(steps),
            on_compaction: None,
            snapshot_max_bytes: None,
        }
    }

    #[test]
    fn a_compaction_accepted_event_marks_the_journal_pending() {
        let state = machine(journal("c1", 0, false), TriggerConfig::default());
        let result = evaluate_transitions(state, ReflectionEvent::CompactionAccepted);
        assert_eq!(result.state.journal.state.pending_compaction, Some(true));
        assert_eq!(result.action, EvaluationAction::None);
    }

    #[test]
    fn a_manual_event_reserves_manual_work() {
        let state = machine(journal("c1", 0, false), TriggerConfig::default());
        let result = evaluate_transitions(
            state,
            ReflectionEvent::Manual {
                focus: Some("thread".to_string()),
                recent_n: Some(5),
                conversation_ids: None,
            },
        );
        match result.action {
            EvaluationAction::Reserve(request) => {
                assert_eq!(request.trigger, ReflectionTrigger::Manual);
                assert_eq!(request.conversation_ids, vec!["c1"]);
                assert_eq!(request.focus.as_deref(), Some("thread"));
                assert_eq!(request.recent_n, Some(5));
            }
            other => panic!("expected a reservation, got {other:?}"),
        }
    }

    #[test]
    fn a_manual_event_can_name_its_conversations() {
        let state = machine(journal("c1", 0, false), TriggerConfig::default());
        let result = evaluate_transitions(
            state,
            ReflectionEvent::Manual {
                focus: None,
                recent_n: None,
                conversation_ids: Some(vec!["a".to_string(), "a".to_string()]),
            },
        );
        match result.action {
            EvaluationAction::Reserve(request) => {
                assert_eq!(request.conversation_ids, vec!["a"]);
            }
            other => panic!("expected a reservation, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_conversation_list_falls_back_to_the_current_one() {
        let state = machine(journal("c1", 0, false), TriggerConfig::default());
        let result = evaluate_transitions(
            state,
            ReflectionEvent::Manual {
                focus: None,
                recent_n: None,
                conversation_ids: Some(vec![]),
            },
        );
        match result.action {
            EvaluationAction::Reserve(request) => {
                assert_eq!(request.conversation_ids, vec!["c1"]);
            }
            other => panic!("expected a reservation, got {other:?}"),
        }
    }

    #[test]
    fn a_failed_settle_reserves_nothing() {
        let state = machine(journal("c1", 99, true), threshold(1));
        let result = evaluate_transitions(state, ReflectionEvent::Settled { success: false });
        assert_eq!(result.action, EvaluationAction::None);
    }

    #[test]
    fn compaction_outranks_the_step_threshold() {
        let config = TriggerConfig {
            step_count: Some(1),
            on_compaction: Some(true),
            snapshot_max_bytes: None,
        };
        let state = machine(journal("c1", 5, true), config);
        let result = evaluate_transitions(state, ReflectionEvent::Settled { success: true });
        match result.action {
            EvaluationAction::Reserve(request) => {
                assert_eq!(request.trigger, ReflectionTrigger::Compaction);
            }
            other => panic!("expected a reservation, got {other:?}"),
        }
    }

    #[test]
    fn compaction_is_skipped_when_the_feature_is_off() {
        let config = TriggerConfig {
            step_count: Some(1),
            on_compaction: Some(false),
            snapshot_max_bytes: None,
        };
        let state = machine(journal("c1", 5, true), config);
        let result = evaluate_transitions(state, ReflectionEvent::Settled { success: true });
        match result.action {
            EvaluationAction::Reserve(request) => {
                assert_eq!(request.trigger, ReflectionTrigger::StepCount);
            }
            other => panic!("expected a reservation, got {other:?}"),
        }
    }

    #[test]
    fn the_step_threshold_reserves_step_count_work() {
        let state = machine(journal("c1", 10, false), threshold(10));
        let result = evaluate_transitions(state, ReflectionEvent::Settled { success: true });
        match result.action {
            EvaluationAction::Reserve(request) => {
                assert_eq!(request.trigger, ReflectionTrigger::StepCount);
            }
            other => panic!("expected a reservation, got {other:?}"),
        }
    }

    #[test]
    fn below_the_threshold_nothing_is_reserved() {
        let state = machine(journal("c1", 9, false), threshold(10));
        let result = evaluate_transitions(state, ReflectionEvent::Settled { success: true });
        assert_eq!(result.action, EvaluationAction::None);
    }

    #[test]
    fn a_zero_threshold_never_fires() {
        let state = machine(journal("c1", 10, false), threshold(0));
        let result = evaluate_transitions(state, ReflectionEvent::Settled { success: true });
        assert_eq!(result.action, EvaluationAction::None);
    }

    #[test]
    fn an_active_run_of_the_same_trigger_suppresses_a_second_reservation() {
        let mut state = machine(journal("c1", 10, false), threshold(10));
        state.reservation.active = Some(ReservedRun {
            run_id: "r1".to_string(),
            request: make_request(
                &journal("c1", 10, false),
                ReflectionTrigger::StepCount,
                vec![],
                None,
                None,
            ),
            reserved_at: None,
            launcher_pid: None,
            launcher_hostname: None,
            launcher_process_start: None,
        });
        let result = evaluate_transitions(state, ReflectionEvent::Settled { success: true });
        assert_eq!(result.action, EvaluationAction::None);
    }

    #[test]
    fn pressure_reserves_a_dream_when_unreflected_bytes_exceed_the_budget() {
        let mut state = machine(journal("c1", 0, false), TriggerConfig::default());
        state.journal.state.unreflected_bytes = Some(REFLECTION_SNAPSHOT_MAX_BYTES);
        let result = evaluate_transitions(state, ReflectionEvent::Settled { success: true });
        match result.action {
            EvaluationAction::Reserve(request) => {
                assert_eq!(request.trigger, ReflectionTrigger::Dream);
                assert_eq!(request.origin, Some(DreamOrigin::Pressure));
            }
            other => panic!("expected a reservation, got {other:?}"),
        }
    }

    #[test]
    fn pressure_stays_quiet_below_the_budget() {
        let mut state = machine(journal("c1", 0, false), TriggerConfig::default());
        state.journal.state.unreflected_bytes = Some(REFLECTION_SNAPSHOT_MAX_BYTES - 1);
        let result = evaluate_transitions(state, ReflectionEvent::Settled { success: true });
        assert_eq!(result.action, EvaluationAction::None);
    }

    #[test]
    fn an_explicit_byte_budget_overrides_the_default() {
        let mut state = machine(
            journal("c1", 0, false),
            TriggerConfig {
                step_count: None,
                on_compaction: None,
                snapshot_max_bytes: Some(10),
            },
        );
        state.journal.state.unreflected_bytes = Some(10);
        let result = evaluate_transitions(state, ReflectionEvent::Settled { success: true });
        assert!(matches!(
            result.action,
            EvaluationAction::Reserve(ReflectionRequest {
                trigger: ReflectionTrigger::Dream,
                ..
            })
        ));
    }

    #[test]
    fn the_first_reservation_becomes_active() {
        let request = make_request(
            &journal("c1", 0, false),
            ReflectionTrigger::Manual,
            vec!["c1".to_string()],
            None,
            None,
        );
        let (state, role) = reserve_transition(ReservationState::default(), request, "r1".into());
        assert_eq!(role, "active");
        assert_eq!(state.active.as_ref().map(|r| r.run_id.as_str()), Some("r1"));
        assert!(state.pending.is_none());
    }

    #[test]
    fn a_second_reservation_becomes_pending() {
        let request = || {
            make_request(
                &journal("c1", 0, false),
                ReflectionTrigger::Manual,
                vec!["c1".to_string()],
                None,
                None,
            )
        };
        let (state, _) = reserve_transition(ReservationState::default(), request(), "r1".into());
        let (state, role) = reserve_transition(state, request(), "r2".into());
        assert_eq!(role, "pending");
        assert_eq!(state.active.as_ref().map(|r| r.run_id.as_str()), Some("r1"));
        assert_eq!(
            state.pending.as_ref().map(|r| r.run_id.as_str()),
            Some("r2")
        );
    }

    #[test]
    fn a_third_reservation_merges_into_the_existing_pending_run() {
        let request = |focus: &str| {
            make_request(
                &journal("c1", 0, false),
                ReflectionTrigger::Manual,
                vec!["c1".to_string()],
                Some(focus.to_string()),
                None,
            )
        };
        let (state, _) = reserve_transition(ReservationState::default(), request("a"), "r1".into());
        let (state, _) = reserve_transition(state, request("b"), "r2".into());
        let (state, _) = reserve_transition(state, request("c"), "r3".into());
        let pending = state.pending.unwrap();
        assert_eq!(pending.run_id, "r2");
        assert_eq!(pending.request.focus.as_deref(), Some("c"));
    }

    #[test]
    fn a_stronger_trigger_wins_the_merge() {
        let mut weak = make_request(
            &journal("c1", 0, false),
            ReflectionTrigger::StepCount,
            vec!["c1".to_string()],
            None,
            None,
        );
        weak.focus = Some("weak".to_string());
        let mut strong = make_request(
            &journal("c1", 0, false),
            ReflectionTrigger::Manual,
            vec!["c1".to_string()],
            None,
            None,
        );
        strong.focus = Some("strong".to_string());
        let merged = merge_requests(weak, strong);
        assert_eq!(merged.trigger, ReflectionTrigger::Manual);
        assert_eq!(merged.focus.as_deref(), Some("strong"));
    }

    #[test]
    fn merging_keeps_both_conversation_sets() {
        let left = make_request(
            &journal("c1", 0, false),
            ReflectionTrigger::Manual,
            vec!["a".to_string()],
            None,
            None,
        );
        let right = make_request(
            &journal("c2", 0, false),
            ReflectionTrigger::Manual,
            vec!["b".to_string()],
            None,
            None,
        );
        let merged = merge_requests(left, right);
        assert_eq!(merged.conversation_ids, vec!["a", "b"]);
    }

    #[test]
    fn merging_takes_the_larger_recent_n() {
        let left = make_request(
            &journal("c1", 0, false),
            ReflectionTrigger::Manual,
            vec![],
            None,
            Some(3),
        );
        let right = make_request(
            &journal("c1", 0, false),
            ReflectionTrigger::Manual,
            vec![],
            None,
            Some(7),
        );
        assert_eq!(merge_requests(left, right).recent_n, Some(7));
    }

    #[test]
    fn merging_drops_a_stale_origin_when_the_trigger_is_not_a_dream() {
        let mut left = make_request(
            &journal("c1", 0, false),
            ReflectionTrigger::Dream,
            vec![],
            None,
            None,
        );
        left.origin = Some(DreamOrigin::Idle);
        let right = make_request(
            &journal("c1", 0, false),
            ReflectionTrigger::Manual,
            vec![],
            None,
            None,
        );
        assert!(merge_requests(left, right).origin.is_none());
    }

    #[test]
    fn pending_is_capped_by_conversation_count() {
        let ids: Vec<String> = (0..REFLECTION_PENDING_MAX_CONVERSATIONS + 5)
            .map(|i| format!("c{i}"))
            .collect();
        let mut request = make_request(
            &journal("c0", 0, false),
            ReflectionTrigger::Manual,
            ids.clone(),
            None,
            None,
        );
        request.snapshots = ids
            .iter()
            .map(|id| CapturedConversation {
                conversation_id: id.clone(),
                snapshot: snapshot(1),
            })
            .collect();
        let (state, _) = reserve_transition(ReservationState::default(), request, "active".into());
        let extra = make_request(
            &journal("c0", 0, false),
            ReflectionTrigger::Manual,
            ids.clone(),
            None,
            None,
        );
        let (state, _) = reserve_transition(state, extra.clone(), "pending".into());
        let (state, _) = reserve_transition(state, extra, "merged".into());
        let pending = state.pending.unwrap();
        assert_eq!(
            pending.request.conversation_ids.len(),
            REFLECTION_PENDING_MAX_CONVERSATIONS
        );
    }

    #[test]
    fn capping_drops_the_oldest_conversation_first() {
        let ids: Vec<String> = (0..REFLECTION_PENDING_MAX_CONVERSATIONS + 1)
            .map(|i| format!("c{i}"))
            .collect();
        let mut request = make_request(
            &journal("c0", 0, false),
            ReflectionTrigger::Manual,
            ids,
            None,
            None,
        );
        request.snapshots = (0..REFLECTION_PENDING_MAX_CONVERSATIONS + 1)
            .map(|i| CapturedConversation {
                conversation_id: format!("c{i}"),
                snapshot: snapshot(1),
            })
            .collect();
        let (state, _) = reserve_transition(ReservationState::default(), request, "r1".into());
        let (state, _) = reserve_transition(
            state,
            make_request(
                &journal("c0", 0, false),
                ReflectionTrigger::Manual,
                vec![],
                None,
                None,
            ),
            "r2".into(),
        );
        let pending = state.pending.unwrap();
        assert!(!pending.request.conversation_ids.contains(&"c0".to_string()));
        assert!(
            pending
                .request
                .snapshots
                .iter()
                .all(|s| s.conversation_id != "c0")
        );
    }

    fn journals(entries: &[(&str, usize, bool)]) -> BTreeMap<String, JournalSnapshot> {
        entries
            .iter()
            .map(|(id, steps, pending)| (id.to_string(), journal(id, *steps, *pending)))
            .collect()
    }

    #[test]
    fn completing_an_unknown_run_is_an_error() {
        let error = complete_transition(
            ReservationState::default(),
            "missing",
            ReflectionOutcome::Merged,
            &BTreeMap::new(),
            &TriggerConfig::default(),
        )
        .unwrap_err();
        assert!(error.contains("not active"), "{error}");
    }

    #[test]
    fn a_merged_run_releases_the_snapshots_for_finalization() {
        let request = make_request(
            &journal("c1", 0, false),
            ReflectionTrigger::Manual,
            vec!["c1".to_string()],
            None,
            None,
        );
        let (state, _) = reserve_transition(ReservationState::default(), request, "r1".into());
        let transition = complete_transition(
            state,
            "r1",
            ReflectionOutcome::Merged,
            &journals(&[("c1", 0, false)]),
            &TriggerConfig::default(),
        )
        .unwrap();
        assert_eq!(transition.finalize.len(), 1);
        assert!(transition.state.active.is_none());
    }

    #[test]
    fn a_failed_run_finalizes_nothing() {
        let request = make_request(
            &journal("c1", 0, false),
            ReflectionTrigger::Manual,
            vec!["c1".to_string()],
            None,
            None,
        );
        let (state, _) = reserve_transition(ReservationState::default(), request, "r1".into());
        let transition = complete_transition(
            state,
            "r1",
            ReflectionOutcome::Failed,
            &journals(&[("c1", 0, false)]),
            &TriggerConfig::default(),
        )
        .unwrap();
        assert!(transition.finalize.is_empty());
        assert!(transition.state.active.is_none());
    }

    #[test]
    fn no_changes_also_counts_as_success() {
        let request = make_request(
            &journal("c1", 0, false),
            ReflectionTrigger::Manual,
            vec!["c1".to_string()],
            None,
            None,
        );
        let (state, _) = reserve_transition(ReservationState::default(), request, "r1".into());
        let transition = complete_transition(
            state,
            "r1",
            ReflectionOutcome::NoChanges,
            &journals(&[("c1", 0, false)]),
            &TriggerConfig::default(),
        )
        .unwrap();
        assert_eq!(transition.finalize.len(), 1);
    }

    #[test]
    fn a_compaction_run_clears_the_pending_flag() {
        let request = make_request(
            &journal("c1", 0, true),
            ReflectionTrigger::Compaction,
            vec!["c1".to_string()],
            None,
            None,
        );
        let (state, _) = reserve_transition(ReservationState::default(), request, "r1".into());
        let transition = complete_transition(
            state,
            "r1",
            ReflectionOutcome::Merged,
            &journals(&[("c1", 0, true)]),
            &TriggerConfig::default(),
        )
        .unwrap();
        assert_eq!(transition.clear_pending_compaction, vec!["c1"]);
    }

    #[test]
    fn a_non_compaction_run_clears_nothing() {
        let request = make_request(
            &journal("c1", 0, true),
            ReflectionTrigger::Manual,
            vec!["c1".to_string()],
            None,
            None,
        );
        let (state, _) = reserve_transition(ReservationState::default(), request, "r1".into());
        let transition = complete_transition(
            state,
            "r1",
            ReflectionOutcome::Merged,
            &journals(&[("c1", 0, true)]),
            &TriggerConfig::default(),
        )
        .unwrap();
        assert!(transition.clear_pending_compaction.is_empty());
    }

    #[test]
    fn a_pending_manual_run_always_launches() {
        let make = || {
            make_request(
                &journal("c1", 0, false),
                ReflectionTrigger::Manual,
                vec!["c1".to_string()],
                None,
                None,
            )
        };
        let (state, _) = reserve_transition(ReservationState::default(), make(), "r1".into());
        let (state, _) = reserve_transition(state, make(), "r2".into());
        let transition = complete_transition(
            state,
            "r1",
            ReflectionOutcome::Merged,
            &journals(&[("c1", 0, false)]),
            &TriggerConfig::default(),
        )
        .unwrap();
        assert_eq!(transition.launch.map(|r| r.run_id), Some("r2".to_string()));
    }

    #[test]
    fn a_pending_step_count_run_is_dropped_when_the_cursor_advanced() {
        let make = || {
            make_request(
                &journal("c1", 5, false),
                ReflectionTrigger::StepCount,
                vec!["c1".to_string()],
                None,
                None,
            )
        };
        let (state, _) = reserve_transition(ReservationState::default(), make(), "r1".into());
        let (state, _) = reserve_transition(state, make(), "r2".into());
        let transition = complete_transition(
            state,
            "r1",
            ReflectionOutcome::Merged,
            &journals(&[("c1", 0, false)]),
            &threshold(10),
        )
        .unwrap();
        assert!(transition.launch.is_none());
        assert!(transition.state.active.is_none());
        assert!(transition.state.pending.is_none());
    }

    #[test]
    fn a_pending_step_count_run_launches_while_the_threshold_holds() {
        let make = || {
            make_request(
                &journal("c1", 20, false),
                ReflectionTrigger::StepCount,
                vec!["c1".to_string()],
                None,
                None,
            )
        };
        let (state, _) = reserve_transition(ReservationState::default(), make(), "r1".into());
        let (state, _) = reserve_transition(state, make(), "r2".into());
        let transition = complete_transition(
            state,
            "r1",
            ReflectionOutcome::Failed,
            &journals(&[("c1", 20, false)]),
            &threshold(10),
        )
        .unwrap();
        assert!(transition.launch.is_some());
    }

    #[test]
    fn a_pending_compaction_run_is_dropped_when_the_flag_cleared() {
        let make = || {
            make_request(
                &journal("c1", 0, true),
                ReflectionTrigger::Compaction,
                vec!["c1".to_string()],
                None,
                None,
            )
        };
        let config = TriggerConfig {
            step_count: None,
            on_compaction: Some(true),
            snapshot_max_bytes: None,
        };
        let (state, _) = reserve_transition(ReservationState::default(), make(), "r1".into());
        let (state, _) = reserve_transition(state, make(), "r2".into());
        let transition = complete_transition(
            state,
            "r1",
            ReflectionOutcome::Merged,
            &journals(&[("c1", 0, true)]),
            &config,
        )
        .unwrap();
        assert!(transition.launch.is_none());
        assert!(transition.state.pending.is_none());
    }

    #[test]
    fn a_pending_compaction_run_launches_for_another_still_pending_conversation() {
        let probe = journals(&[("c1", 0, true), ("c2", 0, true)]);
        let first = make_request(
            &journal("c1", 0, true),
            ReflectionTrigger::Compaction,
            vec!["c1".to_string()],
            None,
            None,
        );
        let second = make_request(
            &journal("c2", 0, true),
            ReflectionTrigger::Compaction,
            vec!["c2".to_string()],
            None,
            None,
        );
        let (state, _) = reserve_transition(ReservationState::default(), first, "r1".into());
        let (state, _) = reserve_transition(state, second, "r2".into());
        let config = TriggerConfig {
            step_count: None,
            on_compaction: Some(true),
            snapshot_max_bytes: None,
        };
        let transition =
            complete_transition(state, "r1", ReflectionOutcome::Merged, &probe, &config).unwrap();
        assert_eq!(transition.clear_pending_compaction, vec!["c1"]);
        assert_eq!(transition.launch.map(|r| r.run_id), Some("r2".to_string()));
    }

    #[test]
    fn the_pending_run_promotes_to_active_with_its_own_id() {
        let make = || {
            make_request(
                &journal("c1", 0, false),
                ReflectionTrigger::Manual,
                vec!["c1".to_string()],
                None,
                None,
            )
        };
        let (state, _) = reserve_transition(ReservationState::default(), make(), "r1".into());
        let (state, _) = reserve_transition(state, make(), "r2".into());
        let transition = complete_transition(
            state,
            "r1",
            ReflectionOutcome::Merged,
            &journals(&[("c1", 0, false)]),
            &TriggerConfig::default(),
        )
        .unwrap();
        assert_eq!(
            transition.state.active.as_ref().map(|r| r.run_id.as_str()),
            Some("r2")
        );
    }
}
