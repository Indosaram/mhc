use super::completion_contracts::ReflectionCompletionRecord;
use memory_core::reflection::{CompletionResult, ReservationState, ReservedRun};

pub trait ReflectionReservationPort {
    fn read_state(&self) -> Result<ReservationState, String>;
    fn read_state_with_wait(
        &self,
        _wait_timeout_ms: Option<u64>,
    ) -> Result<ReservationState, memory_core::reflection::ReservationError> {
        self.read_state()
            .map_err(memory_core::reflection::ReservationError::Lock)
    }
    fn complete(
        &self,
        run_id: &str,
        outcome: memory_core::reflection::ReflectionOutcome,
    ) -> Result<CompletionResult, String>;
    /// Completes a run with explicit promotion control. Ports that only implement `complete` keep
    /// the default promotion behavior; the store-backed port forwards the options.
    fn complete_with_options(
        &self,
        run_id: &str,
        outcome: memory_core::reflection::ReflectionOutcome,
        options: memory_core::reflection::CompleteOptions,
    ) -> Result<CompletionResult, String> {
        let _ = options;
        self.complete(run_id, outcome)
    }
}

pub struct ReflectionRunResult {
    pub run_id: String,
    pub outcome: String,
    pub reason: Option<String>,
    pub detail: Option<String>,
    pub completion: ReflectionCompletionRecord,
    pub launch: Option<ReservedRun>,
}

pub struct ExecutionResult {
    pub outcome: String,
    pub reason: Option<String>,
    pub detail: Option<String>,
    pub model: Option<String>,
    pub thinking: Option<String>,
}
