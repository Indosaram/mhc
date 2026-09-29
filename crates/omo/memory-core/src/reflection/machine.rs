//! Deterministic reflection state machine managing triggers, reservations, and completions.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::journal::cursor::{
    ReflectionSnapshot, ReflectionTranscriptState, count_completed_steps,
};

/// Reflection trigger types that initiate or queue background runs.
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

/// Origin mode for dream triggers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DreamOrigin {
    #[serde(rename = "manual")]
    Manual,
    #[serde(rename = "idle")]
    Idle,
    #[serde(rename = "shutdown")]
    Shutdown,
}

/// Final outcome of an integrated reflection run.
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

/// Trigger thresholds configuration for step count and compaction boundaries.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_compaction: Option<bool>,
}

/// In-memory snapshot of a conversation journal for transition evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalSnapshot {
    pub conversation_id: String,
    pub state: ReflectionTranscriptState,
    pub snapshot: Option<ReflectionSnapshot>,
}

/// Snapshot captured for a single conversation during reservation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturedConversation {
    #[serde(rename = "conversationId")]
    pub conversation_id: String,
    pub snapshot: ReflectionSnapshot,
}

/// Request parameters specifying the work to perform during a reflection run.
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

/// Persisted record of an active or pending reflection run.
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

/// Combined active and pending reservation state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReservationState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<ReservedRun>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<ReservedRun>,
}

/// Input state supplied to evaluate transitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineState {
    pub journal: JournalSnapshot,
    pub reservation: ReservationState,
    pub config: TriggerConfig,
}

/// Event delivered to the reflection state machine.
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

/// Action produced by transition evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvaluationAction {
    None,
    Reserve(ReflectionRequest),
}

/// Result returned from evaluating machine transitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluationResult {
    pub state: MachineState,
    pub action: EvaluationAction,
}

/// State transition details upon completing a reflection run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompleteTransition {
    pub state: ReservationState,
    pub finalize: Vec<CapturedConversation>,
    pub clear_pending_compaction: Vec<String>,
    pub launch: Option<ReservedRun>,
}

/// Evaluates state machine transitions against incoming boundary events.
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
                EvaluationResult {
                    state,
                    action: EvaluationAction::Reserve(request),
                }
            } else {
                EvaluationResult {
                    state,
                    action: EvaluationAction::None,
                }
            }
        }
    }
}

/// Reserves a reflection run, making it active or merging it into pending.
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

    state.pending = Some(pending);
    (state, "pending")
}

/// Finalizes an active reflection run, updating cursors and promoting pending runs.
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

    let active_run = state.active.take().unwrap();
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
            DreamOrigin::Shutdown => 2.5,
            DreamOrigin::Manual => 3.0,
        },
    }
}

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
    let mut seen = BTreeMap::new();
    let mut out = Vec::new();
    for v in values {
        if !seen.contains_key(&v) {
            seen.insert(v.clone(), ());
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
#[path = "machine_tests.rs"]
mod tests;
