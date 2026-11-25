//! senpi `modes/rpc/compact-recovery-base-index.ts`: the bounded row table for a cut's
//! loaded transcript base (S11c, RULING-71 (2)).

use crate::compact_recovery_bytes::{RecoveryBytesError, start_at_or_before};
use maho_core::session_transcript_journal::PersistedBaseError;

/// RULING-61 Q1 = C: 8 MiB transcript ancestry budget.
pub const DEFAULT_TRANSCRIPT_ANCESTRY_BYTES: usize = 8 * 1024 * 1024;
/// The base row table's budget: 419,430 dense rows before coarsening to k-row anchor groups.
pub const BASE_ROW_TABLE_BYTES: usize = DEFAULT_TRANSCRIPT_ANCESTRY_BYTES;

const ANCHOR_BYTES: usize = 20;
const CLOSING_BYTES: usize = 8;
const FNV_OFFSET: u32 = 0x811c_9dc5;
const FNV_PRIME: u32 = 0x0100_0193;

/// A line's byte span in the session file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineSpan {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkMode {
    Record,
    Verify,
}

fn fnv1a(mut hash: u32, bytes: &[u8]) -> u32 {
    for &b in bytes {
        hash = (hash ^ u32::from(b)).wrapping_mul(FNV_PRIME);
    }
    hash
}

pub struct BaseRowTable {
    pub rows_per_anchor: usize,
    pub count: usize,
    anchors: usize,
    starts: Vec<usize>,
    file_starts: Vec<usize>,
    hashes: Vec<u32>,
    /// Where the last counted line ends: a file shorter than this is truncated under the cut.
    pub file_end: usize,
}

impl BaseRowTable {
    pub fn new(count: usize, budget: usize) -> Result<Self, String> {
        let fits = budget.saturating_sub(CLOSING_BYTES) / ANCHOR_BYTES;
        if budget < CLOSING_BYTES + ANCHOR_BYTES || fits < 1 {
            return Err(format!("base row table budget too small: {budget}"));
        }
        let rows_per_anchor = count.div_ceil(fits).max(1);
        let anchors = count.div_ceil(rows_per_anchor);
        Ok(Self {
            rows_per_anchor,
            count,
            anchors,
            starts: vec![0; anchors + 1],
            file_starts: vec![0; anchors],
            hashes: vec![0; anchors],
            file_end: 0,
        })
    }

    /// The table's charge against its budget: `20 x anchors + 8` bytes.
    pub fn table_bytes(&self) -> usize {
        self.starts.len() * 8 + self.file_starts.len() * 8 + self.hashes.len() * 4
    }

    /// The stamped stream's length, known once the open recorded every row.
    pub fn length(&self) -> usize {
        self.starts[self.anchors]
    }

    /// A pass from the anchor at or before `offset` (the open records from 0).
    pub fn walk(&mut self, offset: usize, mode: WalkMode) -> RowWalk<'_> {
        let k = self.rows_per_anchor;
        let group = match mode {
            WalkMode::Record => 0,
            WalkMode::Verify => start_at_or_before(&self.starts, self.anchors, offset),
        };
        let at = self.starts.get(group).copied().unwrap_or(0);
        let file_start = self.file_starts.get(group).copied().unwrap_or(0);
        RowWalk {
            table: self,
            mode,
            group,
            hash: FNV_OFFSET,
            row: group * k,
            at,
            file_start,
        }
    }
}

pub struct RowWalk<'a> {
    table: &'a mut BaseRowTable,
    mode: WalkMode,
    group: usize,
    hash: u32,
    pub row: usize,
    pub at: usize,
    pub file_start: usize,
}

impl RowWalk<'_> {
    pub fn add(&mut self, line: LineSpan, row_bytes: &[u8]) -> Result<bool, RecoveryBytesError> {
        let k = self.table.rows_per_anchor;
        if self.row.is_multiple_of(k) {
            match self.mode {
                WalkMode::Record => {
                    self.table.file_starts[self.group] = line.start;
                    self.table.starts[self.group] = self.at;
                }
                WalkMode::Verify => {
                    if line.start != self.table.file_starts[self.group] {
                        return Err(PersistedBaseError::BaseFingerprintMismatch.into());
                    }
                }
            }
            self.hash = FNV_OFFSET;
        }
        self.hash = fnv1a(self.hash, row_bytes);
        self.row += 1;
        self.at += row_bytes.len();
        if self.mode == WalkMode::Record {
            self.table.file_end = line.end;
        }
        if !self.row.is_multiple_of(k) && self.row != self.table.count {
            return Ok(false);
        }
        match self.mode {
            WalkMode::Record => {
                self.table.hashes[self.group] = self.hash;
                self.table.starts[self.group + 1] = self.at;
            }
            WalkMode::Verify => {
                if self.hash != self.table.hashes[self.group]
                    || self.at != self.table.starts[self.group + 1]
                {
                    return Err(PersistedBaseError::BaseFingerprintMismatch.into());
                }
            }
        }
        self.group += 1;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enforces_budget_by_coarsening_rows_per_anchor() {
        // Budget of 108 bytes -> (108 - 8) / 20 = 5 anchors. 12 rows -> ceil(12/5) = 3 rows/anchor -> 4 anchors.
        let table = BaseRowTable::new(12, 108).expect("table");
        assert_eq!(table.rows_per_anchor, 3);
        assert!(table.table_bytes() <= 108);
    }

    #[test]
    fn records_and_verifies_groups_and_catches_tampering() {
        let mut table = BaseRowTable::new(4, 48).expect("table");
        let lines = [(0, 10), (11, 20), (21, 30), (31, 40)];
        let rows: Vec<Vec<u8>> = (0..4).map(|i| format!("row-{i}\n").into_bytes()).collect();
        {
            let mut walk = table.walk(0, WalkMode::Record);
            for (span, row) in lines.iter().zip(&rows) {
                walk.add(
                    LineSpan {
                        start: span.0,
                        end: span.1,
                    },
                    row,
                )
                .expect("record");
            }
        }
        assert_eq!(table.file_end, 40);
        assert_eq!(table.length(), rows.iter().map(Vec::len).sum::<usize>());

        // Verify valid pass
        {
            let mut walk = table.walk(0, WalkMode::Verify);
            for (span, row) in lines.iter().zip(&rows) {
                walk.add(
                    LineSpan {
                        start: span.0,
                        end: span.1,
                    },
                    row,
                )
                .expect("verify");
            }
        }

        // Tamper one row inside group 0
        let mut walk = table.walk(0, WalkMode::Verify);
        walk.add(LineSpan { start: 0, end: 10 }, b"TAMPR\n")
            .expect("group still open");
        let err = walk
            .add(LineSpan { start: 11, end: 20 }, &rows[1])
            .expect_err("tamper detected at group close");
        assert_eq!(
            err,
            RecoveryBytesError::Base(PersistedBaseError::BaseFingerprintMismatch)
        );
    }
}
