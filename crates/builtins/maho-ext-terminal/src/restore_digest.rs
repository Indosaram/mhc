//! Port of senpi `terminal/restore-digest.ts`: the ONE restore digest a generation sends after a
//! restart - a single sentence naming every monitor and background session, structured details for
//! the renderer, and a pending slot so a digest decided before a model is bound is delivered later.
//!
//! The native `restore::RestoreDigest` carries counters only; per-monitor results arrive here as
//! `RestoreDigestInput`, the upstream digest shape this module reads.
use crate::monitor_status::format_elapsed_seconds;
use serde::Serialize;

pub const RESTORE_DIGEST_CUSTOM_TYPE: &str = "senpi-terminal:restore-digest";
const PREFIX: &str = "Terminal state after restart";
const BUCKETS: [&str; 6] = [
    "restored",
    "muted",
    "lost",
    "completed",
    "attachedElsewhere",
    "expired",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RestoreDigestOutcome {
    Decided,
    Deferred,
    Corrupt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OrphanAction {
    Killed,
    Unverified,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Orphan {
    pub pid: u32,
    pub action: OrphanAction,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorRestoreResult {
    pub monitor_id: String,
    pub description: String,
    /// `command` or `file` (upstream `runtimeKind`).
    pub kind: String,
    /// One of `BUCKETS`.
    pub outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orphan: Option<Orphan>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BackgroundRestoreResult {
    pub id: String,
    pub command: String,
    /// `running` or `exited`.
    pub outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RestoreDigestInput {
    pub store_error: bool,
    pub results: Vec<MonitorRestoreResult>,
    pub background_sessions: Vec<BackgroundRestoreResult>,
    pub downtime_ms: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreDigestDetails {
    pub generation: u64,
    pub outcome: RestoreDigestOutcome,
    pub downtime_ms: f64,
    pub downtime_is_upper_bound: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub holder: Option<serde_json::Value>,
    pub actionable: bool,
    pub monitors: Vec<MonitorRestoreResult>,
    pub background_sessions: Vec<BackgroundRestoreResult>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RestoreDigestMessage {
    pub content: String,
    pub details: RestoreDigestDetails,
}

/// Wake the agent only when something needs a decision: a lost watch, an orphan, a live job.
pub fn is_actionable(digest: &RestoreDigestInput) -> bool {
    digest
        .results
        .iter()
        .any(|result| result.outcome == "lost" || result.orphan.is_some())
        || digest
            .background_sessions
            .iter()
            .any(|session| session.outcome == "running")
}

fn label(result: &MonitorRestoreResult) -> String {
    format!("{} \"{}\"", result.monitor_id, result.description)
}

fn outcome_name(outcome: &str) -> String {
    let mut name = String::new();
    for character in outcome.chars() {
        if character.is_ascii_uppercase() {
            name.push(' ');
            name.push(character.to_ascii_lowercase());
        } else {
            name.push(character);
        }
    }
    name
}

fn downtime_note(ms: f64) -> String {
    if ms > 0.0 {
        format!(" (offline up to {})", format_elapsed_seconds(ms / 1000.0))
    } else {
        String::new()
    }
}

fn orphan_note(orphan: &Orphan) -> String {
    format!(
        "orphan pid {} {}",
        orphan.pid,
        if orphan.action == OrphanAction::Killed {
            "killed"
        } else {
            "left running"
        }
    )
}

fn sentence_entry(result: &MonitorRestoreResult) -> String {
    let revived = result.outcome == "restored" || result.outcome == "muted";
    let fallback = revived.then(|| {
        if result.kind == "file" {
            "rechecked"
        } else {
            "respawned"
        }
    });
    let notes: Vec<String> = [
        result.reason.clone().or(fallback.map(str::to_owned)),
        result.orphan.as_ref().map(orphan_note),
    ]
    .into_iter()
    .flatten()
    .filter(|note| !note.is_empty())
    .collect();
    if notes.is_empty() {
        label(result)
    } else {
        format!("{} ({})", label(result), notes.join(", "))
    }
}

fn background_entry(session: &BackgroundRestoreResult) -> String {
    if session.outcome == "exited" {
        return format!("{} exited", session.id);
    }
    let pid = session
        .pid
        .map_or_else(String::new, |pid| format!("pid {pid}, "));
    format!(
        "{} \"{}\" still running ({pid}output not captured)",
        session.id, session.command
    )
}

fn decided_sentence(digest: &RestoreDigestInput) -> String {
    let mut clauses = Vec::new();
    for outcome in BUCKETS {
        let entries: Vec<String> = digest
            .results
            .iter()
            .filter(|result| result.outcome == outcome)
            .map(sentence_entry)
            .collect();
        if !entries.is_empty() {
            clauses.push(format!(
                "{} {} - {}",
                outcome_name(outcome),
                entries.len(),
                entries.join(", ")
            ));
        }
    }
    if !digest.background_sessions.is_empty() {
        clauses.push(format!(
            "background: {}",
            digest
                .background_sessions
                .iter()
                .map(background_entry)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let body = if clauses.is_empty() {
        "nothing to restore".to_owned()
    } else {
        clauses.join("; ")
    };
    format!("{PREFIX}{}: {body}.", downtime_note(digest.downtime_ms))
}

pub fn build_restore_digest(
    digest: &RestoreDigestInput,
    generation: u64,
    outcome: RestoreDigestOutcome,
    holder_pid: Option<u32>,
) -> RestoreDigestMessage {
    let outcome = if outcome == RestoreDigestOutcome::Decided && digest.store_error {
        RestoreDigestOutcome::Corrupt
    } else {
        outcome
    };
    let content = match outcome {
        RestoreDigestOutcome::Deferred => match holder_pid {
            None => format!(
                "{PREFIX}: waiting for the session lease; monitors come back here once it is acquired."
            ),
            Some(pid) => format!(
                "{PREFIX}: monitors held by pid {pid}; they come back here when that process exits."
            ),
        },
        RestoreDigestOutcome::Corrupt => {
            format!("{PREFIX}: the saved monitor state was unreadable, nothing was restored.")
        }
        RestoreDigestOutcome::Decided => decided_sentence(digest),
    };
    RestoreDigestMessage {
        content,
        details: RestoreDigestDetails {
            generation,
            outcome,
            downtime_ms: digest.downtime_ms,
            downtime_is_upper_bound: true,
            holder: holder_pid.map(|pid| serde_json::json!({ "pid": pid })),
            actionable: is_actionable(digest),
            monitors: digest.results.clone(),
            background_sessions: digest.background_sessions.clone(),
        },
    }
}

/// `createDigestSlot`: exactly one decided digest per generation; a deferred note never displaces
/// a decided digest still waiting for a model.
#[derive(Default)]
pub struct DigestSlot {
    pending: Option<RestoreDigestMessage>,
    decided_delivered: bool,
}

fn is_decided(message: &RestoreDigestMessage) -> bool {
    message.details.outcome != RestoreDigestOutcome::Deferred
}

impl DigestSlot {
    pub fn set(&mut self, message: RestoreDigestMessage) {
        if self.decided_delivered {
            return;
        }
        if !is_decided(&message) && self.pending.as_ref().is_some_and(is_decided) {
            return;
        }
        self.pending = Some(message);
    }

    pub fn pending(&self) -> Option<&RestoreDigestMessage> {
        self.pending.as_ref()
    }

    /// Deliver the pending message; `deliver` returning false keeps it pending.
    pub fn flush(&mut self, deliver: impl FnOnce(&RestoreDigestMessage) -> bool) -> bool {
        let Some(message) = self.pending.as_ref() else {
            return false;
        };
        if !deliver(message) {
            return false;
        }
        let message = self.pending.take().expect("pending");
        if is_decided(&message) {
            self.decided_delivered = true;
        }
        true
    }
}

pub fn create_digest_slot() -> DigestSlot {
    DigestSlot::default()
}

/// `deliverRestoreDigest`: false (message stays pending) while no model is bound. The caller
/// posts the returned custom message through `sendMessage` with the returned options.
pub fn deliver_restore_digest(
    has_model: bool,
    notify: Option<&dyn Fn(&str)>,
    message: &RestoreDigestMessage,
    mut send: impl FnMut(maho_ext_api::CustomMessage, maho_ext_api::SendMessageOptions),
) -> bool {
    if !has_model {
        return false;
    }
    if let Some(notify) = notify {
        notify(&message.content);
    }
    let actionable = message.details.actionable;
    send(
        maho_ext_api::CustomMessage {
            custom_type: RESTORE_DIGEST_CUSTOM_TYPE.to_owned(),
            content: vec![maho_tools::definition::ToolContent::text(message.content.clone())],
            display: true,
            details: serde_json::to_value(&message.details).ok(),
        },
        maho_ext_api::SendMessageOptions {
            trigger_turn: actionable,
            deliver_as: Some(if actionable {
                maho_ext_api::DeliverAs::FollowUp
            } else {
                maho_ext_api::DeliverAs::NextTurn
            }),
        },
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(id: &str, outcome: &str, kind: &str) -> MonitorRestoreResult {
        MonitorRestoreResult {
            monitor_id: id.into(),
            description: "watch".into(),
            kind: kind.into(),
            outcome: outcome.into(),
            command: None,
            path: None,
            reason: None,
            orphan: None,
        }
    }

    #[test]
    fn decided_sentence_names_every_monitor_and_background_session() {
        let mut lost = monitor("mon_2", "lost", "command");
        lost.reason = Some("no restore handler".into());
        let mut killed = monitor("mon_3", "restored", "command");
        killed.orphan = Some(Orphan {
            pid: 9,
            action: OrphanAction::Killed,
        });
        let digest = RestoreDigestInput {
            results: vec![
                monitor("mon_1", "restored", "file"),
                lost,
                killed,
                monitor("mon_4", "attachedElsewhere", "file"),
            ],
            background_sessions: vec![
                BackgroundRestoreResult {
                    id: "bash_1".into(),
                    command: "sleep 9".into(),
                    outcome: "running".into(),
                    pid: Some(5),
                },
                BackgroundRestoreResult {
                    id: "bash_2".into(),
                    command: "x".into(),
                    outcome: "exited".into(),
                    pid: None,
                },
            ],
            downtime_ms: 90_000.0,
            ..Default::default()
        };
        let message = build_restore_digest(&digest, 2, RestoreDigestOutcome::Decided, None);
        assert_eq!(
            message.content,
            "Terminal state after restart (offline up to 1m): restored 2 - mon_1 \"watch\" (rechecked), mon_3 \"watch\" (respawned, orphan pid 9 killed); lost 1 - mon_2 \"watch\" (no restore handler); attached elsewhere 1 - mon_4 \"watch\"; background: bash_1 \"sleep 9\" still running (pid 5, output not captured), bash_2 exited."
        );
        assert!(message.details.actionable);
        assert_eq!(
            build_restore_digest(
                &RestoreDigestInput::default(),
                1,
                RestoreDigestOutcome::Decided,
                None
            )
            .content,
            "Terminal state after restart: nothing to restore."
        );
    }

    #[test]
    fn deferred_and_corrupt_messages() {
        let empty = RestoreDigestInput::default();
        assert!(
            build_restore_digest(&empty, 1, RestoreDigestOutcome::Deferred, Some(42))
                .content
                .contains("held by pid 42")
        );
        assert!(
            build_restore_digest(&empty, 1, RestoreDigestOutcome::Deferred, None)
                .content
                .contains("waiting for the session lease")
        );
        let corrupt = RestoreDigestInput {
            store_error: true,
            ..Default::default()
        };
        let message = build_restore_digest(&corrupt, 1, RestoreDigestOutcome::Decided, None);
        assert_eq!(message.details.outcome, RestoreDigestOutcome::Corrupt);
        assert!(!message.details.actionable);
    }

    #[test]
    fn slot_keeps_one_decided_digest_per_generation() {
        let empty = RestoreDigestInput::default();
        let mut slot = create_digest_slot();
        slot.set(build_restore_digest(
            &empty,
            1,
            RestoreDigestOutcome::Decided,
            None,
        ));
        slot.set(build_restore_digest(
            &empty,
            1,
            RestoreDigestOutcome::Deferred,
            None,
        ));
        assert_eq!(
            slot.pending().unwrap().details.outcome,
            RestoreDigestOutcome::Decided
        );
        assert!(!slot.flush(|_| false));
        assert!(slot.pending().is_some());
        assert!(slot.flush(|_| true));
        slot.set(build_restore_digest(
            &empty,
            1,
            RestoreDigestOutcome::Decided,
            None,
        ));
        assert!(slot.pending().is_none());
    }

    #[test]
    fn delivery_waits_for_a_model_and_routes_actionable_digests_as_follow_up() {
        let digest = RestoreDigestInput {
            results: vec![monitor("mon_1", "lost", "command")],
            ..Default::default()
        };
        let message = build_restore_digest(&digest, 1, RestoreDigestOutcome::Decided, None);
        assert!(!deliver_restore_digest(
            false,
            None,
            &message,
            |_, _| panic!("no send")
        ));
        let mut sent = Vec::new();
        assert!(deliver_restore_digest(
            true,
            None,
            &message,
            |custom, options| sent.push((
                custom.custom_type,
                options.trigger_turn,
                options.deliver_as
            ))
        ));
        assert_eq!(
            sent,
            vec![(
                RESTORE_DIGEST_CUSTOM_TYPE.to_owned(),
                true,
                Some(maho_ext_api::DeliverAs::FollowUp)
            )]
        );
    }
}
