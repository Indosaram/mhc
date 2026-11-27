//! Port of senpi packages/coding-agent/src/core/session-move/move-record.ts.
//!
//! Re-exports the canonical record, answer, status, decline/fail reasons, and fence error
//! from `move_ledger` so downstream crates keep one type identity.

pub use super::move_ledger::{
    DeclineReason, FailReason, MoveAnswer, MoveFencedError, MoveRecord, MoveStatus,
    parse_move_status, parse_record,
};
