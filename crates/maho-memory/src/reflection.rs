//! Reflection and dream scheduling for the memory store.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-reflection.md`. A settled turn may reserve at most
//! one active and one pending run; the compaction, step-count and pressure boundaries are checked
//! in that order, and a manual trigger is always reserved. Dream policy bounds what a dream run may
//! add to a person's observations.
use serde::{Deserialize, Serialize};
use std::fmt;

/// Conversations held back for a pending run before new ones are dropped.
pub const PENDING_MAX_CONVERSATIONS: usize = 32;

/// Bytes of pending conversation snapshots kept before a pressure dream is forced.
pub const PENDING_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Bytes of unreflected journal data that force a pressure dream.
pub const SNAPSHOT_MAX_BYTES: u64 = 2 * 1024 * 1024;

/// What started a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ReflectionTrigger {
    /// Enough turns have settled since the last successful run.
    #[serde(rename = "step-count")]
    StepCount,
    /// A compaction was accepted since the last run.
    #[serde(rename = "compaction")]
    Compaction,
    /// A person or a command asked for it.
    #[serde(rename = "manual")]
    Manual,
    /// The dream boundary was reached or requested.
    #[serde(rename = "dream")]
    Dream,
}

/// What prompted a dream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DreamOrigin {
    /// A person asked for it.
    #[serde(rename = "manual")]
    Manual,
    /// The session went idle.
    #[serde(rename = "idle")]
    Idle,
    /// The session shut down.
    #[serde(rename = "shutdown")]
    Shutdown,
    /// Unreflected bytes reached the budget.
    #[serde(rename = "pressure")]
    Pressure,
}

impl DreamOrigin {
    /// Whether a dream from this origin may run without being asked for.
    pub fn is_automatic(self) -> bool {
        matches!(self, Self::Idle | Self::Shutdown | Self::Pressure)
    }
}

/// How a finished run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ReflectionOutcome {
    /// The run's commits were merged into the store.
    #[serde(rename = "merged")]
    Merged,
    /// The run finished and had nothing to change.
    #[serde(rename = "no_changes")]
    NoChanges,
    /// The parent store had uncommitted changes, so the run was not merged.
    #[serde(rename = "parent_dirty")]
    ParentDirty,
    /// The merge failed on a conflict.
    #[serde(rename = "merge_conflict")]
    MergeConflict,
    /// The run left uncommitted changes behind.
    #[serde(rename = "dirty_uncommitted")]
    DirtyUncommitted,
    /// The run failed.
    #[serde(rename = "failed")]
    Failed,
    /// The run exceeded its deadline.
    #[serde(rename = "timed_out")]
    TimedOut,
}

impl ReflectionOutcome {
    /// Whether this outcome counts as a failure for the consecutive-failure streak.
    pub fn is_failure(self) -> bool {
        !matches!(self, Self::Merged | Self::NoChanges)
    }

    /// Whether this outcome reflects new material into the store without a merge.
    pub fn is_success(self) -> bool {
        matches!(self, Self::Merged | Self::NoChanges)
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

/// Thresholds that decide when a run is worth reserving.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TriggerConfig {
    /// Settled turns between step-count runs; `None` and `0` both disable it.
    pub step_count: Option<usize>,
    /// Whether an accepted compaction reserves a run.
    pub on_compaction: Option<bool>,
    /// Unreflected bytes that force a pressure dream.
    pub snapshot_max_bytes: Option<u64>,
}

/// A conversation's journal as the scheduler sees it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JournalSnapshot {
    /// The conversation the events belong to.
    pub conversation_id: String,
    /// Settled turns since the last successful run.
    pub steps_since_last_successful_reflection: usize,
    /// Unreflected journal bytes.
    pub unreflected_bytes: Option<u64>,
    /// Whether a compaction was accepted since the last run.
    pub pending_compaction: bool,
}

/// One conversation captured for a run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturedConversation {
    /// The conversation the snapshot came from.
    #[serde(rename = "conversationId")]
    pub conversation_id: String,
    /// The journal bytes handed to the run.
    #[serde(rename = "snapshotBytes", default)]
    pub snapshot_bytes: u64,
}

/// A reserved run: what it should look at and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReflectionRequest {
    /// Why the run was reserved.
    pub trigger: ReflectionTrigger,
    /// Only present for dream triggers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<DreamOrigin>,
    /// Conversations the run should read.
    #[serde(rename = "conversationIds")]
    pub conversation_ids: Vec<String>,
    /// Optional focus text from a manual trigger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<String>,
    /// Optional number of recent conversations a manual trigger asked for.
    #[serde(rename = "recentN", default, skip_serializing_if = "Option::is_none")]
    pub recent_n: Option<usize>,
    /// Conversations captured at reservation time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub captured: Vec<CapturedConversation>,
}

/// A run holding one of the two reservation slots.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReservedRun {
    /// `reflection-run-<n>`.
    #[serde(rename = "runId")]
    pub run_id: String,
    /// What the run was reserved for.
    pub request: ReflectionRequest,
    /// When the reservation was made, as an RFC 3339 instant.
    #[serde(
        rename = "reservedAt",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub reserved_at: Option<String>,
    /// The process that made the reservation.
    #[serde(
        rename = "launcherPid",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub launcher_pid: Option<u32>,
}

/// The active and pending reservation slots.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReservationState {
    /// The run currently executing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<ReservedRun>,
    /// The run waiting for the active one to finish.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<ReservedRun>,
}

impl ReservationState {
    /// Whether a trigger already holds one of the slots.
    pub fn contains(&self, trigger: ReflectionTrigger) -> bool {
        self.active
            .iter()
            .chain(self.pending.iter())
            .any(|run| run.request.trigger == trigger)
    }

    /// Reserve a run, or queue it when the active slot is taken.
    pub fn reserve(&mut self, run: ReservedRun) {
        if self.active.is_none() {
            self.active = Some(run);
        } else {
            self.pending = Some(run);
        }
    }

    /// Release the active run, promoting the pending one, and return what was released.
    pub fn release(&mut self) -> Option<ReservedRun> {
        let released = self.active.take();
        self.active = self.pending.take();
        released
    }
}

/// Scheduler state: the journal counters plus the reservation slots.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MachineState {
    /// The conversation being scheduled for.
    pub journal: JournalSnapshot,
    /// The two reservation slots.
    pub reservation: ReservationState,
    /// The thresholds in force.
    pub config: TriggerConfig,
}

/// An event that may reserve a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReflectionEvent {
    /// A turn finished.
    Settled {
        /// Whether the turn finished cleanly; a failed turn reserves nothing.
        success: bool,
    },
    /// A compaction was accepted.
    CompactionAccepted,
    /// A person asked for a run.
    Manual {
        /// Optional focus text.
        focus: Option<String>,
        /// Optional number of recent conversations to read.
        recent_n: Option<usize>,
        /// Optional explicit conversations; defaults to the current one.
        conversation_ids: Option<Vec<String>>,
    },
}

/// What a scheduler evaluation decided to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EvaluationAction {
    /// Nothing to run.
    None,
    /// Reserve a run for this request.
    Reserve(ReflectionRequest),
}

/// The result of one evaluation: the new state and what to do next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvaluationResult {
    /// State after the event was applied.
    pub state: MachineState,
    /// The reservation to make, if any.
    pub action: EvaluationAction,
}

/// What a completed run changed in the scheduler state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompleteTransition {
    /// Reservation slots after the completion, with the pending run promoted.
    pub state: ReservationState,
    /// The run that finished, if one was active.
    pub finished: Option<ReservedRun>,
    /// Whether the compaction flag should be cleared.
    pub clear_pending_compaction: bool,
    /// The promoted run, if one was waiting.
    pub launch: Option<ReservedRun>,
}

fn make_request(
    journal: &JournalSnapshot,
    trigger: ReflectionTrigger,
    conversation_ids: Vec<String>,
    focus: Option<String>,
    recent_n: Option<usize>,
) -> ReflectionRequest {
    ReflectionRequest {
        trigger,
        origin: None,
        conversation_ids,
        focus,
        recent_n,
        captured: vec![CapturedConversation {
            conversation_id: journal.conversation_id.clone(),
            snapshot_bytes: journal.unreflected_bytes.unwrap_or(0),
        }],
    }
}

/// Apply one event to the scheduler state.
pub fn evaluate_transitions(mut state: MachineState, event: ReflectionEvent) -> EvaluationResult {
    match event {
        ReflectionEvent::CompactionAccepted => {
            state.journal.pending_compaction = true;
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
                Some(list) if !list.is_empty() => list,
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
                && state.journal.pending_compaction
                && !state.reservation.contains(ReflectionTrigger::Compaction);
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
                && state.journal.steps_since_last_successful_reflection >= threshold
                && !state.reservation.contains(ReflectionTrigger::StepCount);
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
                .unwrap_or(SNAPSHOT_MAX_BYTES);
            let pressure_ready = budget > 0
                && state
                    .journal
                    .unreflected_bytes
                    .is_some_and(|bytes| bytes >= budget)
                && !state.reservation.contains(ReflectionTrigger::StepCount);
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

/// Record the completion of a run and reset the counters its outcome allows.
pub fn complete_transition(
    mut state: MachineState,
    outcome: ReflectionOutcome,
) -> (MachineState, CompleteTransition) {
    let finished = state.reservation.release();
    let clear_pending_compaction = outcome.is_success()
        && finished
            .as_ref()
            .is_some_and(|run| run.request.trigger == ReflectionTrigger::Compaction);

    if outcome.is_success() {
        state.journal.steps_since_last_successful_reflection = 0;
        state.journal.unreflected_bytes = Some(0);
    }
    if clear_pending_compaction {
        state.journal.pending_compaction = false;
    }

    let launch = state.reservation.active.clone();
    let transition = CompleteTransition {
        state: state.reservation.clone(),
        finished,
        clear_pending_compaction,
        launch,
    };
    (state, transition)
}

/// The `people` section of `dream-policy.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DreamPeoplePolicy {
    /// Whether dream runs may write people observations.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Most observations one dream may add.
    #[serde(default = "default_max_entries")]
    pub max_entries: usize,
    /// Longest observation one dream may add.
    #[serde(default = "default_max_entry_chars")]
    pub max_entry_chars: usize,
}

fn default_enabled() -> bool {
    true
}

fn default_max_entries() -> usize {
    40
}

fn default_max_entry_chars() -> usize {
    200
}

impl Default for DreamPeoplePolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            max_entries: 40,
            max_entry_chars: 200,
        }
    }
}

/// A whole `dream-policy.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DreamPolicy {
    /// Policy schema version.
    #[serde(default = "default_policy_version")]
    pub version: u32,
    /// People dreaming bounds.
    #[serde(default)]
    pub people: DreamPeoplePolicy,
}

fn default_policy_version() -> u32 {
    1
}

impl Default for DreamPolicy {
    fn default() -> Self {
        Self {
            version: 1,
            people: DreamPeoplePolicy::default(),
        }
    }
}

impl DreamPolicy {
    /// Parse a policy, falling back to the defaults for absent fields.
    pub fn parse(source: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(source)
    }

    /// Filter candidate observations down to what this policy allows.
    ///
    /// Entries over the character limit are dropped whole, never truncated mid-sentence, and the
    /// list is cut at `max_entries`.
    pub fn admit(&self, candidates: impl IntoIterator<Item = String>) -> Vec<String> {
        if !self.people.enabled {
            return Vec::new();
        }
        candidates
            .into_iter()
            .filter(|entry| entry.chars().count() <= self.people.max_entry_chars)
            .take(self.people.max_entries)
            .collect()
    }
}

/// `dream-state.json`: when the last dream ran.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DreamState {
    /// When the last dream finished, as an RFC 3339 instant.
    #[serde(
        rename = "last_dream_at",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub last_dream_at: Option<String>,
    /// The run id of the last dream.
    #[serde(rename = "lastRunId", default, skip_serializing_if = "Option::is_none")]
    pub last_run_id: Option<String>,
}

/// One finished run, as written to `reflection-run-<n>.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecord {
    /// Record schema version.
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    /// `reflection-run-<n>`.
    #[serde(rename = "runId")]
    pub run_id: String,
    /// The store identity the run wrote to.
    pub identity: String,
    /// The agent category the run executed as.
    pub category: String,
    /// The model that executed the run.
    pub model: String,
    /// Conversations the run read.
    #[serde(rename = "conversationIds")]
    pub conversation_ids: Vec<String>,
    /// What started the run.
    pub trigger: ReflectionTrigger,
    /// Present only for dream runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<DreamOrigin>,
    /// How the run ended.
    pub outcome: ReflectionOutcome,
    /// When the run started.
    #[serde(rename = "startedAt")]
    pub started_at: String,
    /// When the run finished.
    #[serde(rename = "finishedAt")]
    pub finished_at: String,
    /// Wall-clock duration in milliseconds.
    #[serde(rename = "durationMs")]
    pub duration_ms: u64,
    /// How many files the run changed.
    #[serde(rename = "filesChanged")]
    pub files_changed: usize,
    /// The failure streak after this run.
    #[serde(rename = "consecutiveFailures")]
    pub consecutive_failures: usize,
}

impl RunRecord {
    /// Parse a stored run record.
    pub fn parse(source: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(source)
    }

    /// Render a run record as the store writes it.
    pub fn render(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self).map(|mut json| {
            json.push('\n');
            json
        })
    }

    /// The failure streak after this run, given the streak before it.
    pub fn streak_after(previous: usize, outcome: ReflectionOutcome) -> usize {
        if outcome.is_failure() {
            previous.saturating_add(1)
        } else {
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(config: TriggerConfig) -> MachineState {
        MachineState {
            journal: JournalSnapshot {
                conversation_id: "c1".to_owned(),
                ..JournalSnapshot::default()
            },
            reservation: ReservationState::default(),
            config,
        }
    }

    fn reserved(trigger: ReflectionTrigger, origin: Option<DreamOrigin>) -> ReservedRun {
        ReservedRun {
            run_id: "reflection-run-1".to_owned(),
            request: ReflectionRequest {
                trigger,
                origin,
                conversation_ids: vec!["c1".to_owned()],
                focus: None,
                recent_n: None,
                captured: Vec::new(),
            },
            reserved_at: None,
            launcher_pid: None,
        }
    }

    #[test]
    fn a_failed_turn_reserves_nothing() {
        let mut state = state(TriggerConfig {
            step_count: Some(1),
            on_compaction: Some(true),
            snapshot_max_bytes: Some(1),
        });
        state.journal.steps_since_last_successful_reflection = 5;
        state.journal.pending_compaction = true;
        state.journal.unreflected_bytes = Some(1_000);
        let result = evaluate_transitions(state, ReflectionEvent::Settled { success: false });
        assert_eq!(result.action, EvaluationAction::None);
    }

    #[test]
    fn a_compaction_wins_over_a_step_count_boundary() {
        let mut state = state(TriggerConfig {
            step_count: Some(1),
            on_compaction: Some(true),
            snapshot_max_bytes: None,
        });
        state.journal.steps_since_last_successful_reflection = 9;
        state.journal.pending_compaction = true;
        let result = evaluate_transitions(state, ReflectionEvent::Settled { success: true });
        match result.action {
            EvaluationAction::Reserve(request) => {
                assert_eq!(request.trigger, ReflectionTrigger::Compaction);
                assert_eq!(request.conversation_ids, ["c1"]);
            }
            EvaluationAction::None => panic!("a compaction should reserve a run"),
        }
    }

    #[test]
    fn a_reserved_compaction_is_not_reserved_twice() {
        let mut state = state(TriggerConfig {
            step_count: None,
            on_compaction: Some(true),
            snapshot_max_bytes: None,
        });
        state.journal.pending_compaction = true;
        state.reservation.active = Some(reserved(ReflectionTrigger::Compaction, None));
        let result = evaluate_transitions(state, ReflectionEvent::Settled { success: true });
        assert_eq!(result.action, EvaluationAction::None);
    }

    #[test]
    fn a_step_count_boundary_reserves_a_run() {
        let mut state = state(TriggerConfig {
            step_count: Some(3),
            on_compaction: None,
            snapshot_max_bytes: None,
        });
        state.journal.steps_since_last_successful_reflection = 3;
        let result = evaluate_transitions(state, ReflectionEvent::Settled { success: true });
        match result.action {
            EvaluationAction::Reserve(request) => {
                assert_eq!(request.trigger, ReflectionTrigger::StepCount);
                assert_eq!(request.origin, None);
            }
            EvaluationAction::None => panic!("reaching the threshold should reserve a run"),
        }
    }

    #[test]
    fn a_step_count_of_zero_is_disabled() {
        let mut state = state(TriggerConfig {
            step_count: Some(0),
            on_compaction: None,
            snapshot_max_bytes: None,
        });
        state.journal.steps_since_last_successful_reflection = 100;
        let result = evaluate_transitions(state, ReflectionEvent::Settled { success: true });
        assert_eq!(result.action, EvaluationAction::None);
    }

    #[test]
    fn unreflected_bytes_past_the_budget_reserve_a_pressure_dream() {
        let mut state = state(TriggerConfig {
            step_count: None,
            on_compaction: None,
            snapshot_max_bytes: Some(1_000),
        });
        state.journal.unreflected_bytes = Some(1_000);
        let result = evaluate_transitions(state, ReflectionEvent::Settled { success: true });
        match result.action {
            EvaluationAction::Reserve(request) => {
                assert_eq!(request.trigger, ReflectionTrigger::Dream);
                assert_eq!(request.origin, Some(DreamOrigin::Pressure));
            }
            EvaluationAction::None => panic!("pressure should reserve a dream"),
        }
    }

    #[test]
    fn a_manual_trigger_is_reserved_while_another_run_is_active() {
        let mut state = state(TriggerConfig::default());
        state.reservation.active = Some(reserved(ReflectionTrigger::StepCount, None));
        let result = evaluate_transitions(
            state,
            ReflectionEvent::Manual {
                focus: Some("people only".to_owned()),
                recent_n: Some(5),
                conversation_ids: None,
            },
        );
        match result.action {
            EvaluationAction::Reserve(request) => {
                assert_eq!(request.trigger, ReflectionTrigger::Manual);
                assert_eq!(request.focus.as_deref(), Some("people only"));
                assert_eq!(request.recent_n, Some(5));
                assert_eq!(request.conversation_ids, ["c1"]);
            }
            EvaluationAction::None => panic!("manual runs are always reserved"),
        }
    }

    #[test]
    fn a_manual_trigger_naming_conversations_keeps_them() {
        let result = evaluate_transitions(
            state(TriggerConfig::default()),
            ReflectionEvent::Manual {
                focus: None,
                recent_n: None,
                conversation_ids: Some(vec!["a".to_owned(), "b".to_owned()]),
            },
        );
        match result.action {
            EvaluationAction::Reserve(request) => {
                assert_eq!(request.conversation_ids, ["a", "b"]);
            }
            EvaluationAction::None => panic!("manual runs are always reserved"),
        }
    }

    #[test]
    fn a_successful_run_resets_the_step_count_and_unreflected_bytes() {
        let mut state = state(TriggerConfig::default());
        state.reservation.active = Some(reserved(ReflectionTrigger::StepCount, None));
        state.journal.steps_since_last_successful_reflection = 4;
        state.journal.unreflected_bytes = Some(900);
        let (state, transition) = complete_transition(state, ReflectionOutcome::Merged);
        assert_eq!(state.journal.steps_since_last_successful_reflection, 0);
        assert_eq!(state.journal.unreflected_bytes, Some(0));
        assert!(transition.finished.is_some());
    }

    #[test]
    fn a_failed_run_keeps_the_step_count_but_clears_the_slots() {
        let mut state = state(TriggerConfig::default());
        state.reservation.active = Some(reserved(ReflectionTrigger::StepCount, None));
        state.journal.steps_since_last_successful_reflection = 4;
        let (state, transition) = complete_transition(state, ReflectionOutcome::Failed);
        assert_eq!(state.journal.steps_since_last_successful_reflection, 4);
        assert!(state.reservation.active.is_none());
        assert!(transition.finished.is_some());
    }

    #[test]
    fn a_finished_compaction_run_clears_the_pending_compaction_flag() {
        let mut state = state(TriggerConfig::default());
        state.reservation.active = Some(reserved(ReflectionTrigger::Compaction, None));
        state.journal.pending_compaction = true;
        let (state, transition) = complete_transition(state, ReflectionOutcome::Merged);
        assert!(!state.journal.pending_compaction);
        assert!(transition.clear_pending_compaction);
    }

    #[test]
    fn a_failed_compaction_run_leaves_the_pending_compaction_flag_set() {
        let mut state = state(TriggerConfig::default());
        state.reservation.active = Some(reserved(ReflectionTrigger::Compaction, None));
        state.journal.pending_compaction = true;
        let (state, transition) = complete_transition(state, ReflectionOutcome::TimedOut);
        assert!(state.journal.pending_compaction);
        assert!(!transition.clear_pending_compaction);
    }

    #[test]
    fn a_pending_run_is_promoted_when_the_active_one_finishes() {
        let mut state = state(TriggerConfig::default());
        state.reservation.active = Some(reserved(ReflectionTrigger::Manual, None));
        state.reservation.pending = Some(ReservedRun {
            run_id: "reflection-run-2".to_owned(),
            request: ReflectionRequest {
                trigger: ReflectionTrigger::StepCount,
                origin: None,
                conversation_ids: vec!["c1".to_owned()],
                focus: None,
                recent_n: None,
                captured: Vec::new(),
            },
            reserved_at: None,
            launcher_pid: None,
        });
        let (state, transition) = complete_transition(state, ReflectionOutcome::NoChanges);
        assert_eq!(
            state
                .reservation
                .active
                .as_ref()
                .map(|run| run.run_id.as_str()),
            Some("reflection-run-2")
        );
        assert_eq!(
            transition.launch.as_ref().map(|run| run.run_id.as_str()),
            Some("reflection-run-2")
        );
    }

    #[test]
    fn a_failure_streak_counts_up_and_resets() {
        assert_eq!(RunRecord::streak_after(2, ReflectionOutcome::Failed), 3);
        assert_eq!(RunRecord::streak_after(2, ReflectionOutcome::TimedOut), 3);
        assert_eq!(RunRecord::streak_after(2, ReflectionOutcome::NoChanges), 0);
        assert_eq!(RunRecord::streak_after(2, ReflectionOutcome::Merged), 0);
    }

    #[test]
    fn only_automatic_dream_origins_are_automatic() {
        assert!(DreamOrigin::Pressure.is_automatic());
        assert!(DreamOrigin::Idle.is_automatic());
        assert!(DreamOrigin::Shutdown.is_automatic());
        assert!(!DreamOrigin::Manual.is_automatic());
    }

    #[test]
    fn a_dream_policy_parses_the_stored_shape() {
        let policy = DreamPolicy::parse(
            r#"{
  "version": 1,
  "people": { "enabled": true, "max_entries": 40, "max_entry_chars": 200 }
}"#,
        )
        .expect("policy");
        assert_eq!(policy, DreamPolicy::default());
        assert_eq!(policy.people.max_entries, 40);
    }

    #[test]
    fn a_disabled_people_policy_admits_nothing() {
        let mut policy = DreamPolicy::default();
        policy.people.enabled = false;
        assert!(policy.admit(["Anything".to_owned()]).is_empty());
    }

    #[test]
    fn over_long_entries_are_dropped_whole_never_truncated() {
        let mut policy = DreamPolicy::default();
        policy.people.max_entry_chars = 5;
        let admitted = policy.admit([
            "short".to_owned(),
            "far too long".to_owned(),
            "ok".to_owned(),
        ]);
        assert_eq!(admitted, ["short", "ok"]);
    }

    #[test]
    fn a_people_policy_caps_how_many_entries_one_dream_adds() {
        let mut policy = DreamPolicy::default();
        policy.people.max_entries = 2;
        let admitted = policy.admit((0..5).map(|index| format!("entry {index}")));
        assert_eq!(admitted, ["entry 0", "entry 1"]);
    }

    #[test]
    fn a_run_record_round_trips_through_the_stored_json() {
        let record = RunRecord {
            schema_version: 1,
            run_id: "reflection-run-22".to_owned(),
            identity: "indo-b00481cc".to_owned(),
            category: "memory".to_owned(),
            model: "mahoquot/gpt-6-luna".to_owned(),
            conversation_ids: vec!["01a14c88".to_owned()],
            trigger: ReflectionTrigger::Dream,
            origin: Some(DreamOrigin::Shutdown),
            outcome: ReflectionOutcome::NoChanges,
            started_at: "2026-11-10T08:16:07.119Z".to_owned(),
            finished_at: "2026-11-10T08:17:36.942Z".to_owned(),
            duration_ms: 89_823,
            files_changed: 0,
            consecutive_failures: 0,
        };
        let rendered = record.render().expect("render");
        assert!(rendered.contains("\"trigger\": \"dream\""));
        assert!(rendered.contains("\"outcome\": \"no_changes\""));
        assert!(rendered.contains("\"runId\": \"reflection-run-22\""));
        assert_eq!(RunRecord::parse(&rendered).expect("parse"), record);
    }

    #[test]
    fn a_dream_state_round_trips() {
        let state = DreamState {
            last_dream_at: Some("2026-10-18T02:01:46.608Z".to_owned()),
            last_run_id: Some("reflection-run-18".to_owned()),
        };
        let json = serde_json::to_string(&state).expect("serialize");
        assert_eq!(
            serde_json::from_str::<DreamState>(&json).expect("parse"),
            state
        );
        assert!(json.contains("\"last_dream_at\""));
    }

    #[test]
    fn an_outcome_renders_as_the_store_spells_it() {
        assert_eq!(ReflectionOutcome::NoChanges.to_string(), "no_changes");
        assert_eq!(
            ReflectionOutcome::MergeConflict.to_string(),
            "merge_conflict"
        );
        assert_eq!(ReflectionOutcome::TimedOut.to_string(), "timed_out");
    }
}
