use super::{detached_cell_contract::EvalDetachedCellState, types::EvalLanguage};

pub fn allows_detached_cell_transition(from: EvalDetachedCellState, to: EvalDetachedCellState) -> bool {
    use EvalDetachedCellState::*;
    match from {
        Queued => matches!(to, Running | Failed | Cancelled),
        Running => matches!(to, Completed | Failed | Cancelled),
        Detached | Completed | Failed | Cancelled => false,
    }
}

pub fn detached_cell_is_active(state: EvalDetachedCellState) -> bool {
    matches!(state, EvalDetachedCellState::Queued | EvalDetachedCellState::Running | EvalDetachedCellState::Detached)
}

pub fn active_detached_cell_reuse_error(cell_id: &str, language: EvalLanguage, state: EvalDetachedCellState, detached: bool) -> String {
    let language = match language { EvalLanguage::Js => "js", EvalLanguage::Py => "py", EvalLanguage::Rb => "rb", EvalLanguage::Jl => "jl" };
    let state = if detached { "detached" } else { state.as_str() };
    format!("Eval cell {cell_id} from a previous call is still {state} in the {language} kernel. Use eval({{ action: \"peek\", cell_id: \"{cell_id}\" }}) to read it or eval({{ action: \"stop\", cell_id: \"{cell_id}\" }}) to end it before its id can be reused.")
}
