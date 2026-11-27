//! Port of `packages/coding-agent/src/core/kill-guard/pattern.ts` (senpi#5349).

use super::ere::{PatternReading, read_pattern};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PatternMode {
    Exact,
    Substring,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PatternVerdict {
    Narrow,
    Broad { why: String },
}

impl PatternVerdict {
    pub const fn is_broad(&self) -> bool {
        matches!(self, Self::Broad { .. })
    }
}

/// R1 on one pattern: our own ERE reader, independent of the host engine (amendment A2's literal-count rule).
pub fn judge_pattern(pattern: &str, mode: PatternMode) -> PatternVerdict {
    let reading = read_pattern(pattern);
    match reading {
        PatternReading::Unparsable { why } => PatternVerdict::Broad {
            why: format!("cannot be read as a regular expression ({why})"),
        },
        PatternReading::Parsed {
            empty_branch,
            nullable,
            min_literals,
        } => {
            if empty_branch {
                return PatternVerdict::Broad {
                    why: "has an empty alternative, which matches every process".into(),
                };
            }
            if nullable {
                return PatternVerdict::Broad {
                    why: "can match the empty string, so it matches every process".into(),
                };
            }
            let needed = match mode {
                PatternMode::Exact => 1,
                PatternMode::Substring => 2,
            };
            if min_literals >= needed {
                return PatternVerdict::Narrow;
            }
            let plural = if min_literals == 1 { "" } else { "s" };
            PatternVerdict::Broad {
                why: format!(
                    "has a match with {min_literals} literal character{plural}, so it matches nearly every process"
                ),
            }
        }
    }
}
