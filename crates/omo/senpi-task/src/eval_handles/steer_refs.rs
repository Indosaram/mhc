use super::{errors::{EvalHandleErrorCode, EvalHandleHostError}, host::HandleRef, task_refs::{load_fenced, TaskRecord}};
pub const CANCEL_REASON: &str = "cancelled from an eval handle";
pub fn agent_ref(reference: &HandleRef) -> Result<(), EvalHandleHostError> { if matches!(reference.kind, super::host::HandleKind::Agent) { Ok(()) } else { Err(EvalHandleHostError::new(EvalHandleErrorCode::Unsupported, "operation is for agent handles")) } }
pub fn send_allowed(tasks: &[TaskRecord], reference: &HandleRef, owner: &str) -> Result<(), EvalHandleHostError> { agent_ref(reference)?; load_fenced(tasks, reference, owner).map(|_| ()) }
