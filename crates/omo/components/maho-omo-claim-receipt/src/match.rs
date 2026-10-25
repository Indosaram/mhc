//! Port of `claim-receipt/match.ts`: match each claim to the evidence visible when it was made.

use crate::classify::{Claim, ClaimKind};
use crate::evidence::{CheckEvidence, CheckKind, CheckOutcome, EditEvidence, Timeline};

/// What the answer's claim turned out to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ClaimStatus {
    Supported,
    Unsupported,
    Unknown,
    Admitted,
}

impl ClaimStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::Unsupported => "unsupported",
            Self::Unknown => "unknown",
            Self::Admitted => "admitted",
        }
    }
}

/// Why an unsupported claim has no receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UnsupportedReason {
    NoCheck,
    LastCheckFailed,
    StaleAfterEdit,
}

impl UnsupportedReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoCheck => "no-check",
            Self::LastCheckFailed => "last-check-failed",
            Self::StaleAfterEdit => "stale-after-edit",
        }
    }
}

/// The check a verdict rests on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    pub entry_id: String,
    pub kind: CheckKind,
    pub outcome: CheckOutcome,
}

/// The edit that invalidated a passing check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidatedBy {
    pub entry_id: String,
    pub paths: Vec<String>,
}

/// One claim's verdict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimVerdict {
    pub claim: Claim,
    pub status: ClaimStatus,
    pub reason: Option<UnsupportedReason>,
    pub receipt: Option<Receipt>,
    pub invalidated_by: Option<InvalidatedBy>,
}

const ANY_CHECK: [CheckKind; 6] = [
    CheckKind::Test,
    CheckKind::Typecheck,
    CheckKind::Lint,
    CheckKind::Build,
    CheckKind::Run,
    CheckKind::Lsp,
];

fn supported_by(kind: ClaimKind) -> &'static [CheckKind] {
    match kind {
        ClaimKind::Test => &[CheckKind::Test],
        ClaimKind::Typecheck => &[CheckKind::Typecheck, CheckKind::Lsp],
        ClaimKind::Lint => &[CheckKind::Lint],
        ClaimKind::Build => &[CheckKind::Build],
        ClaimKind::Run => &[CheckKind::Run, CheckKind::Test, CheckKind::Build],
        ClaimKind::Exists => &[
            CheckKind::Test,
            CheckKind::Typecheck,
            CheckKind::Lint,
            CheckKind::Build,
            CheckKind::Run,
            CheckKind::Lsp,
            CheckKind::Lookup,
        ],
        ClaimKind::Done => &ANY_CHECK,
    }
}

fn needs_a_code_edit(kind: ClaimKind) -> bool {
    matches!(kind, ClaimKind::Done | ClaimKind::Run)
}

fn receipt_of(check: &CheckEvidence) -> Receipt {
    Receipt {
        entry_id: check.entry_id.clone(),
        kind: check.kind,
        outcome: check.outcome,
    }
}

fn latest_check<'a>(items: &[&'a CheckEvidence]) -> Option<&'a CheckEvidence> {
    let mut best: Option<&'a CheckEvidence> = None;
    for item in items {
        if best.is_none_or(|current| item.at >= current.at) {
            best = Some(item);
        }
    }
    best
}

fn latest_edit<'a>(items: &[&'a EditEvidence]) -> Option<&'a EditEvidence> {
    let mut best: Option<&'a EditEvidence> = None;
    for item in items {
        if best.is_none_or(|current| item.at >= current.at) {
            best = Some(item);
        }
    }
    best
}

fn judge(claim: &Claim, checks: &[CheckEvidence], edits: &[EditEvidence]) -> ClaimVerdict {
    let base = ClaimVerdict {
        claim: claim.clone(),
        status: ClaimStatus::Unknown,
        reason: None,
        receipt: None,
        invalidated_by: None,
    };
    if claim.admitted {
        return ClaimVerdict {
            status: ClaimStatus::Admitted,
            ..base
        };
    }
    if needs_a_code_edit(claim.kind) && !edits.iter().any(|edit| edit.relevant) {
        return base;
    }
    let supporting = supported_by(claim.kind);
    let candidates: Vec<&CheckEvidence> = checks
        .iter()
        .filter(|check| supporting.contains(&check.kind))
        .collect();
    if claim.kind == ClaimKind::Exists {
        return judge_exists(claim, &candidates, checks.len(), base);
    }
    let Some(last) = latest_check(&candidates) else {
        // Check runs of other kinds mean verification happened somewhere this session cannot see
        // (remote runners, CI); only a session with no check run at all, or a claim that quotes no
        // observed result, is flagged. A lookup (reading or searching a file) is never a check run:
        // it proves only that something exists.
        let ran_any_check = checks.iter().any(|check| check.kind != CheckKind::Lookup);
        if claim.reported || ran_any_check {
            return base;
        }
        return ClaimVerdict {
            status: ClaimStatus::Unsupported,
            reason: Some(UnsupportedReason::NoCheck),
            ..base
        };
    };
    // A later check whose outcome is unknown cannot erase a failure that was visible before it,
    // unless the claim quotes concrete numbers, which most likely come from that later run's output
    // the timeline could not read.
    let decisive_candidates: Vec<&CheckEvidence> = candidates
        .iter()
        .copied()
        .filter(|check| check.outcome != CheckOutcome::Unknown)
        .collect();
    let decisive = latest_check(&decisive_candidates);
    let quotes_later_run = claim.reported && last.outcome == CheckOutcome::Unknown;
    if decisive.is_some_and(|check| check.outcome == CheckOutcome::Failed) && !quotes_later_run {
        return ClaimVerdict {
            status: ClaimStatus::Unsupported,
            reason: Some(UnsupportedReason::LastCheckFailed),
            receipt: decisive.map(receipt_of),
            ..base
        };
    }
    if last.outcome == CheckOutcome::Unknown {
        return ClaimVerdict {
            receipt: Some(receipt_of(last)),
            ..base
        };
    }
    let stale_candidates: Vec<&EditEvidence> = edits
        .iter()
        .filter(|edit| edit.relevant && edit.at > last.at)
        .collect();
    if let Some(stale) = latest_edit(&stale_candidates) {
        return ClaimVerdict {
            status: ClaimStatus::Unsupported,
            reason: Some(UnsupportedReason::StaleAfterEdit),
            receipt: Some(receipt_of(last)),
            invalidated_by: Some(InvalidatedBy {
                entry_id: stale.entry_id.clone(),
                paths: stale.paths.clone(),
            }),
            ..base
        };
    }
    ClaimVerdict {
        status: ClaimStatus::Supported,
        receipt: Some(receipt_of(last)),
        ..base
    }
}

// Existence is proven by looking (read, grep, symbol lookups) or by any passing check; a failing
// check of an unrelated kind says nothing about whether something exists, so it never flags the
// claim.
fn judge_exists(
    claim: &Claim,
    candidates: &[&CheckEvidence],
    check_count: usize,
    base: ClaimVerdict,
) -> ClaimVerdict {
    let passing: Vec<&CheckEvidence> = candidates
        .iter()
        .copied()
        .filter(|check| check.outcome == CheckOutcome::Passed)
        .collect();
    if let Some(proof) = latest_check(&passing) {
        return ClaimVerdict {
            status: ClaimStatus::Supported,
            receipt: Some(receipt_of(proof)),
            ..base
        };
    }
    if claim.reported || check_count > 0 {
        return base;
    }
    ClaimVerdict {
        status: ClaimStatus::Unsupported,
        reason: Some(UnsupportedReason::NoCheck),
        ..base
    }
}

/// Evaluates each claim at the answer's branch position: evidence at or after `answer_at` is never
/// consulted.
#[must_use]
pub fn match_claims(timeline: &Timeline, claims: &[Claim], answer_at: f64) -> Vec<ClaimVerdict> {
    let checks: Vec<CheckEvidence> = timeline
        .checks
        .iter()
        .filter(|check| check.at < answer_at)
        .cloned()
        .collect();
    let edits: Vec<EditEvidence> = timeline
        .edits
        .iter()
        .filter(|edit| edit.at < answer_at)
        .cloned()
        .collect();
    claims
        .iter()
        .map(|claim| judge(claim, &checks, &edits))
        .collect()
}

#[must_use]
pub fn is_flagged(verdict: &ClaimVerdict) -> bool {
    verdict.status == ClaimStatus::Unsupported
}
