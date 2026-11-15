//! Port of omo `packages/omo-senpi/src/components/task/judge-runner.ts`.

pub use senpi_task::manager::parent_registry_context::find_model_reference;
pub use senpi_task::runners::in_process::child_handle::RunnerFailureKind;
pub use senpi_task::runners::in_process::runner_error::RunnerError;

pub fn is_model_unavailable_start(error: &RunnerError) -> bool {
    error.kind() == RunnerFailureKind::ModelUnavailable
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_model_unavailable_failure_counts_as_an_unavailable_start() {
        assert!(is_model_unavailable_start(&RunnerError::new(
            RunnerFailureKind::ModelUnavailable,
            "selector excluded"
        )));
        assert!(!is_model_unavailable_start(&RunnerError::new(
            RunnerFailureKind::DepthExceeded,
            "too deep"
        )));
    }
}
