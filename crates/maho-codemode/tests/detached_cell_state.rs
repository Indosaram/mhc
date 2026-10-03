use maho_codemode::tool::{detached_cell_contract::EvalDetachedCellState::*, detached_cell_state::*};

#[test]
fn exhaustive_transition_matrix() {
    let states = [Queued, Running, Detached, Completed, Failed, Cancelled];
    for from in states {
        for to in states {
            let expected = matches!((from, to), (Queued, Running | Failed | Cancelled) | (Running, Completed | Failed | Cancelled));
            assert_eq!(allows_detached_cell_transition(from, to), expected, "{from:?} -> {to:?}");
        }
    }
}

#[test]
fn active_state_includes_legacy_detached() {
    for state in [Queued, Running, Detached] { assert!(detached_cell_is_active(state)); }
    for state in [Completed, Failed, Cancelled] { assert!(!detached_cell_is_active(state)); }
}

#[test]
fn states_have_wire_discriminants() {
    for state in [Queued, Running, Detached, Completed, Failed, Cancelled] {
        assert_eq!(serde_json::to_value(state).unwrap(), state.as_str());
    }
}
