//! Port of senpi packages/coding-agent/src/core/input-origin.ts.

use maho_ext_api::InputSource;

use crate::manual_continue::MANUAL_CONTINUE_CUSTOM_TYPE;

/// senpi continue-from-leaf.ts `CONTINUE_FROM_LEAF_CUSTOM_TYPE`.
const CONTINUE_FROM_LEAF_CUSTOM_TYPE: &str = "continue-from-leaf";

/// Who started an input, classified once at its entry point (senpi#3853).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputOrigin {
    /// RPC prompt / steer / follow_up / answer and TUI submit.
    Person,
    /// Extension API sends (`pi.sendUserMessage`).
    Extension,
    /// Goal, monitor, codemode and other automation turns.
    Engine,
}

/// A `.` manual continue or a continue-from-leaf is the user's own request, not an automatic turn
/// (senpi engine-turn-limit.ts `isUserDirectedTurn`).
fn is_user_directed_turn(custom_type: &str) -> bool {
    custom_type == MANUAL_CONTINUE_CUSTOM_TYPE || custom_type == CONTINUE_FROM_LEAF_CUSTOM_TYPE
}

pub fn input_origin_of(source: Option<InputSource>) -> InputOrigin {
    if source == Some(InputSource::Extension) {
        InputOrigin::Extension
    } else {
        InputOrigin::Person
    }
}

pub fn custom_turn_origin(custom_type: &str) -> InputOrigin {
    if is_user_directed_turn(custom_type) {
        InputOrigin::Person
    } else {
        InputOrigin::Engine
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_extension_sources_are_extension_origin() {
        assert_eq!(
            input_origin_of(Some(InputSource::Extension)),
            InputOrigin::Extension
        );
        assert_eq!(input_origin_of(Some(InputSource::Rpc)), InputOrigin::Person);
        assert_eq!(
            input_origin_of(Some(InputSource::Interactive)),
            InputOrigin::Person
        );
        assert_eq!(input_origin_of(None), InputOrigin::Person);
    }

    #[test]
    fn user_directed_custom_turns_are_person_origin() {
        assert_eq!(custom_turn_origin("manual-continue"), InputOrigin::Person);
        assert_eq!(
            custom_turn_origin("continue-from-leaf"),
            InputOrigin::Person
        );
        assert_eq!(custom_turn_origin("goal"), InputOrigin::Engine);
    }
}
