use crate::lifecycle::deferred_revival_reasons::PERMANENT_REVIVAL_REASONS;

#[derive(Default)]
pub struct RecoveryRecord<'a> {
    pub status: &'a str,
    pub residency_state: Option<&'a str>,
    pub suspension_reason: Option<&'a str>,
    pub revival_deferred_reason: Option<&'a str>,
    pub failure_kind: Option<&'a str>,
    pub error_message: Option<&'a str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryState { Resuming, Ending }

pub struct RecoveryPresentation {
    pub state: RecoveryState,
    pub cause: String,
    pub text: String,
}

pub fn recovery_presentation(record: &RecoveryRecord<'_>) -> Option<RecoveryPresentation> {
    if matches!(record.status, "completed" | "error" | "cancelled" | "interrupted" | "lost") {
        return None;
    }
    let expiring = record.failure_kind == Some("suspended_unresumable")
        || record.error_message.is_some_and(|message| message.starts_with("suspended_unresumable:"));
    if !expiring && record.suspension_reason.is_none()
        && matches!(record.residency_state, None | Some("resident")) {
        return None;
    }
    let reason = record.revival_deferred_reason.or(record.suspension_reason).unwrap_or_default();
    let state = if expiring || PERMANENT_REVIVAL_REASONS.contains(&reason) {
        RecoveryState::Ending
    } else {
        RecoveryState::Resuming
    };
    let cause = match record.revival_deferred_reason {
        Some(reason) if record.suspension_reason == Some("revival_deferred") => format!("revival deferred: {reason}"),
        Some(reason) => reason.to_owned(),
        None => match record.suspension_reason {
            None => "parent session restarted",
            Some("daemon_unavailable") => "task daemon unavailable",
            Some("own_host_unreachable") => "host lost",
            Some("host_incompatible") => "host version mismatch",
            Some("revival_deferred") => "revival deferred",
            Some("idle_evicted") => "evicted while idle",
            Some("handoff_parked") => "handed off",
            Some("host_draining") => "host draining",
            Some("session_quit") => "session quit",
            Some("store_index_unavailable") => "task store unavailable",
            Some(other) => other,
        }.to_owned(),
    };
    let label = match state { RecoveryState::Resuming => "resuming", RecoveryState::Ending => "ending" };
    let text = format!("{label} ({cause})");
    Some(RecoveryPresentation { state, cause, text })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_records_never_recover() {
        for status in ["completed", "error", "cancelled", "interrupted", "lost"] {
            assert!(recovery_presentation(&RecoveryRecord {
                status, suspension_reason: Some("host_incompatible"), ..Default::default()
            }).is_none());
        }
    }

    #[test]
    fn ordinary_live_records_are_not_parked() {
        for residency_state in [None, Some("resident")] {
            assert!(recovery_presentation(&RecoveryRecord {
                status: "running", residency_state, ..Default::default()
            }).is_none());
        }
    }

    #[test]
    fn permanent_deferrals_and_expiring_records_end() {
        for reason in PERMANENT_REVIVAL_REASONS {
            let result = recovery_presentation(&RecoveryRecord {
                status: "running", suspension_reason: Some("revival_deferred"),
                revival_deferred_reason: Some(reason), ..Default::default()
            }).expect("parked record");
            assert_eq!(result.state, RecoveryState::Ending);
        }
        let result = recovery_presentation(&RecoveryRecord {
            status: "running", error_message: Some("suspended_unresumable: unavailable"),
            ..Default::default()
        }).expect("expiring record");
        assert_eq!(result.state, RecoveryState::Ending);
    }

    #[test]
    fn detached_and_retryable_records_resume() {
        for residency_state in ["persisted_only", "rpc_detached", "evicted"] {
            let result = recovery_presentation(&RecoveryRecord {
                status: "pending", residency_state: Some(residency_state), ..Default::default()
            }).expect("detached record");
            assert_eq!(result.state, RecoveryState::Resuming);
        }
        let result = recovery_presentation(&RecoveryRecord {
            status: "running", suspension_reason: Some("revival_deferred"),
            revival_deferred_reason: Some("capacity"), ..Default::default()
        }).expect("retryable record");
        assert_eq!(result.state, RecoveryState::Resuming);
    }
}
