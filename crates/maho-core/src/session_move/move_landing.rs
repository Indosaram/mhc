//! Landing metadata and reconciliation for a move's single commit point.
use super::move_ledger::{FailReason, MoveRecord, MoveStatus};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LandingOutcome {
    Absent,
    MovedIn,
    Revoked,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Landing {
    pub target: Option<String>,
    pub committing: bool,
}
pub fn parse_landing(value: &Value) -> Landing {
    Landing {
        target: value
            .get("target")
            .and_then(Value::as_str)
            .map(str::to_owned),
        committing: value.get("committing").and_then(Value::as_bool) == Some(true),
    }
}
pub fn settled_landing(
    record: Option<&MoveRecord>,
    committing: bool,
    outcome: LandingOutcome,
) -> Option<MoveRecord> {
    match record {
        Some(MoveRecord::Status(MoveStatus::Receiving)) if committing => {
            Some(MoveRecord::Status(match outcome {
                LandingOutcome::Absent => MoveStatus::Failed {
                    reason: FailReason::ImportFailed,
                },
                LandingOutcome::MovedIn => MoveStatus::Accepted,
                LandingOutcome::Revoked => MoveStatus::AcceptedRevoked,
            }))
        }
        Some(MoveRecord::Status(MoveStatus::Accepted)) if outcome == LandingOutcome::Revoked => {
            Some(MoveRecord::Status(MoveStatus::AcceptedRevoked))
        }
        _ => None,
    }
}
pub fn lands_at(record: Option<&MoveRecord>, landing: &Landing, target: &str) -> bool {
    matches!(record, Some(MoveRecord::Status(MoveStatus::Receiving)))
        && !landing.committing
        && landing.target.as_deref() == Some(target)
}
pub fn landed_elsewhere(record: Option<&MoveRecord>, landing: &Landing) -> String {
    if matches!(
        record,
        Some(MoveRecord::Status(MoveStatus::AcceptedRevoked))
    ) {
        return "it was revoked here and never lands again".into();
    }
    if landing.committing {
        return "an import stopped mid-commit; only its landed file is accepted".into();
    }
    if matches!(record, Some(MoveRecord::Status(MoveStatus::Accepted))) {
        return "it already landed here once; a move lands once".into();
    }
    landing.target.as_ref().map_or_else(
        || "its landing target is unknown".into(),
        |target| format!("it lands only as {target}"),
    )
}
