//! senpi `modes/rpc/compact-recovery-staleness.ts`: classifies why a once-valid or retired
//! checkpoint token no longer names a readable cut.

use crate::compact_recovery_streams::RecoveryStreamRecord;
use crate::compact_recovery_types::{
    CheckpointToken, RecoveryAdmission, RecoveryFailure, RepairReason, repair_needed,
    superseded_token,
};
use crate::session_acceptance_lane::RetireReason;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneStalenessView {
    pub retired: bool,
    pub retire_reason: Option<RetireReason>,
    pub session_epoch: String,
}

/// What the asker's stream says about its own token no longer owned here: the bounded description it gets.
pub fn retired_token_failure(
    stream: Option<&RecoveryStreamRecord>,
    admission: &RecoveryAdmission,
) -> RecoveryFailure {
    let Some(stream) = stream else {
        return repair_needed(RepairReason::UnknownToken);
    };
    if stream.session_epoch != admission.session_epoch {
        return repair_needed(RepairReason::StaleEpoch);
    }
    if stream.generation != admission.generation {
        return superseded_token(stream.generation);
    }
    repair_needed(RepairReason::UnknownToken)
}

/// Why a once-valid token no longer names a readable cut; `None` while it still does.
pub fn token_staleness(
    stream: Option<&RecoveryStreamRecord>,
    token: &CheckpointToken,
    lane: &LaneStalenessView,
) -> Option<RecoveryFailure> {
    if stream.is_none() && lane.retire_reason == Some(RetireReason::Close) {
        return Some(repair_needed(RepairReason::SessionClosed));
    }
    if lane.retired {
        return Some(repair_needed(RepairReason::LaneRetired));
    }
    let Some(stream) = stream else {
        return Some(repair_needed(RepairReason::PeerGone));
    };
    if lane.session_epoch != token.cut.session_epoch
        || stream.session_epoch != token.admission.session_epoch
    {
        return Some(repair_needed(RepairReason::StaleEpoch));
    }
    if stream.generation != token.admission.generation {
        return Some(superseded_token(stream.generation));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_recovery_types::{RecoveryCutIdentity, RecoveryRouteKind};
    use std::collections::BTreeSet;

    fn sample_token(gen_: u64) -> CheckpointToken {
        CheckpointToken::new(
            "tok-1".into(),
            RecoveryAdmission {
                socket_epoch: "se-1".into(),
                admission_id: "adm-1".into(),
                route_kind: RecoveryRouteKind::Ordinary,
                peer: "peer-1".into(),
                session_id: "s1".into(),
                session_epoch: "ep-1".into(),
                generation: gen_,
            },
            RecoveryCutIdentity {
                session_epoch: "ep-1".into(),
                sequence: 1,
                history_version: 1,
                leaf_id: None,
                active_message_id: None,
                turn_id: None,
            },
        )
    }

    fn stream(ep: &str, gen_: u64) -> RecoveryStreamRecord {
        RecoveryStreamRecord {
            session_id: "s1".into(),
            session_epoch: ep.into(),
            generation: gen_,
            routes: BTreeSet::new(),
            current: Some("tok-1".into()),
        }
    }

    #[test]
    fn classifies_retired_and_live_token_staleness() {
        let tok = sample_token(2);
        assert_eq!(
            retired_token_failure(None, &tok.admission),
            repair_needed(RepairReason::UnknownToken)
        );
        assert_eq!(
            retired_token_failure(Some(&stream("ep-2", 2)), &tok.admission),
            repair_needed(RepairReason::StaleEpoch)
        );
        assert_eq!(
            retired_token_failure(Some(&stream("ep-1", 3)), &tok.admission),
            superseded_token(3)
        );

        let live_lane = LaneStalenessView {
            retired: false,
            retire_reason: None,
            session_epoch: "ep-1".into(),
        };
        assert_eq!(
            token_staleness(Some(&stream("ep-1", 2)), &tok, &live_lane),
            None
        );
        assert_eq!(
            token_staleness(None, &tok, &live_lane),
            Some(repair_needed(RepairReason::PeerGone))
        );
        let closed_lane = LaneStalenessView {
            retired: true,
            retire_reason: Some(RetireReason::Close),
            session_epoch: "ep-1".into(),
        };
        assert_eq!(
            token_staleness(None, &tok, &closed_lane),
            Some(repair_needed(RepairReason::SessionClosed))
        );
    }
}
