use memory_core::reflection::ReservedRun;
use super::completion_contracts::ReflectionCompletionRecord;

type WarnCallback<'a> = dyn Fn(&str, &std::collections::BTreeMap<String, String>) + Send + Sync + 'a;

pub struct RunFinalizationContext<'a> {
    pub identity: &'a memory_core::identity::resolve::MemoryIdentity,
    pub reservation: &'a dyn super::runner_types::ReflectionReservationPort,
    pub launch: Option<&'a dyn Fn(&ReservedRun)>,
    pub now_ms: &'a dyn Fn() -> i64,
    pub receipts: Option<&'a dyn crate::receipts_port::MemoryReceiptsPort>,
    pub warn: Option<&'a WarnCallback<'a>>,
}

pub struct ReservationRunResult {
    pub run_id: String,
    pub outcome: String,
    pub reason: Option<String>,
    pub detail: Option<String>,
    pub completion: Option<ReflectionCompletionRecord>,
    pub launch: Option<ReservedRun>,
}

pub struct DurableFinalizationDecision {
    pub outcome: String,
    pub reason: Option<String>,
    pub detail: Option<String>,
    pub integration_sha: Option<String>,
    /// Published from a validated worktree tip after the supervisor died, not from a recorded child exit.
    pub recovered_from_worktree: bool,
}
