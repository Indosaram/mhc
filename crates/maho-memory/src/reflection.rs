//! Dream scheduling policy for the memory store.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-reflection.md`. Dream policy bounds what a dream run may
//! add to a person's observations.
//!
//! The state machine that decides *when* a run is reserved lives in [`crate::reflection_machine`]; this
//! module only re-exports its vocabulary so a caller holding a dream policy has one path to both.

pub use crate::reflection_machine::{
    DreamOrigin, ReflectionEvent, ReflectionOutcome, ReflectionTrigger,
};

use serde::{Deserialize, Serialize};

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
