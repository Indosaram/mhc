pub const SCOPED_RETRY_REASONS: &[&str] = &[
    "capacity", "lock_contended", "model_unavailable", "session_unavailable",
    "rollback_failed", "foreign_live_owner",
];
pub const LOST_ON_EXHAUSTION: &[&str] = &[
    "model_unavailable", "session_unavailable", "rollback_failed", "lock_contended",
];
pub const PERMANENT_REVIVAL_REASONS: &[&str] = &[
    "host_incompatible", "spawn_spec_unavailable", "transcript_unavailable",
    "isolated_not_revivable",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeferralOutlook { Ending, Recovering }

pub fn deferral_outlook_for(reason: &str, _host_session: bool) -> DeferralOutlook {
    if PERMANENT_REVIVAL_REASONS.contains(&reason) {
        DeferralOutlook::Ending
    } else {
        DeferralOutlook::Recovering
    }
}
