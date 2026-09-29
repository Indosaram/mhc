use crate::host::HostError;
use crate::store::StoreError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResidentSummary {
    pub task_id: String,
    pub name: String,
    pub status: String,
}

/// The residency cap is full and no terminal idle child can be evicted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "Residency cap {max_children} reached for session {session_id}; no terminal idle child is evictable. Residents: {}.",
    named_residents(.residents)
)]
pub struct AgentLimitReached {
    pub max_children: usize,
    pub session_id: String,
    pub residents: Vec<ResidentSummary>,
}

fn named_residents(residents: &[ResidentSummary]) -> String {
    if residents.is_empty() {
        return "none".to_string();
    }
    residents
        .iter()
        .map(|resident| format!("{}({})", resident.name, resident.status))
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Debug, thiserror::Error)]
pub enum LifecycleError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Host(#[from] HostError),
}
