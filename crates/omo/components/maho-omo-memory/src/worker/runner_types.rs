use memory_core::reflection::{CompletionResult, ReservationState, ReservedRun};
use super::completion_contracts::ReflectionCompletionRecord;

pub trait ReflectionReservationPort {
    fn read_state(&self) -> Result<ReservationState, String>;
    fn read_state_with_wait(&self, _wait_timeout_ms: Option<u64>) -> Result<ReservationState, memory_core::reflection::ReservationError> {
        self.read_state().map_err(memory_core::reflection::ReservationError::Lock)
    }
    fn complete(&self, run_id: &str, outcome: memory_core::reflection::ReflectionOutcome) -> Result<CompletionResult, String>;
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
