//! Quarantine terminal sentinel (`quarantined.json`) for unreadable or stranded reflection runs.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::machine::ReservedRun;
use super::reservation::write_json_atomic;

pub const QUARANTINE_FILENAME: &str = "quarantined.json";
pub const RESERVATION_EVIDENCE_FILENAME: &str = "reservation.quarantined.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuarantineReason {
    LedgerUnreadable,
    PrelaunchMissingAfterDeadline,
    TerminalClaimUnrecoverable,
    StrandedNoLedger,
}

impl QuarantineReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LedgerUnreadable => "ledger_unreadable",
            Self::PrelaunchMissingAfterDeadline => "prelaunch_missing_after_deadline",
            Self::TerminalClaimUnrecoverable => "terminal_claim_unrecoverable",
            Self::StrandedNoLedger => "stranded_no_ledger",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuarantineRecord {
    pub version: u32,
    pub run_id: String,
    pub kind: String,
    pub trigger: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// `ledger.startedAt` when the ledger parses, else reservation `reservedAt`.
    pub generation: String,
    pub reason: QuarantineReason,
    pub quarantined_at: String,
    pub evidence: Vec<String>,
}

/// Evidence first, sentinel second; nothing already in the run dir is moved, rewritten or deleted.
pub fn write_quarantine(
    run_dir: &Path,
    record: &QuarantineRecord,
    reservation: Option<&ReservedRun>,
) -> std::io::Result<()> {
    if let Some(res) = reservation {
        write_json_atomic(&run_dir.join(RESERVATION_EVIDENCE_FILENAME), res)?;
    }
    write_json_atomic(&run_dir.join(QUARANTINE_FILENAME), record)
}
