//! Port of senpi packages/coding-agent/src/core/turn-conflict.ts.

use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnConflictError {
    pub current_turn_id: Option<String>,
}

impl TurnConflictError {
    pub fn new(current_turn_id: Option<String>) -> Self {
        Self { current_turn_id }
    }
}

impl fmt::Display for TurnConflictError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.current_turn_id {
            None => {
                f.write_str("No turn is running, so this steer cannot reach the turn it names.")
            }
            Some(id) => write!(
                f,
                "This steer names a turn that is not running; the running turn is {id}."
            ),
        }
    }
}

impl std::error::Error for TurnConflictError {}

pub fn turn_conflict(expected: Option<&str>, current: Option<&str>) -> Option<TurnConflictError> {
    let expected = expected?;
    if Some(expected) == current {
        None
    } else {
        Some(TurnConflictError::new(current.map(str::to_owned)))
    }
}
