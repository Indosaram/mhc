use crate::eval_handles::errors::{EvalHandleErrorCode, EvalHandleHostError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HandleKind { Agent, Workpool }
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HandleRef { pub kind: HandleKind, pub id: String, pub run_epoch: i64 }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandlePhase { Pending, Succeeded, Failed, Cancelled, Lost }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandleSnapshot { pub reference: HandleRef, pub phase: HandlePhase, pub host_status: String, pub revision: i64 }
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandleOutcome { Fulfilled { reference: HandleRef, value: String }, Rejected { reference: HandleRef, code: EvalHandleErrorCode, message: String } }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus { Pending, Running, Paused, Completed, Error, Cancelled, Interrupted, Lost }
pub fn phase(status: TaskStatus) -> HandlePhase { match status { TaskStatus::Pending|TaskStatus::Running|TaskStatus::Paused => HandlePhase::Pending, TaskStatus::Completed => HandlePhase::Succeeded, TaskStatus::Error => HandlePhase::Failed, TaskStatus::Cancelled|TaskStatus::Interrupted => HandlePhase::Cancelled, TaskStatus::Lost => HandlePhase::Lost } }
pub fn snapshot(reference: HandleRef, status: TaskStatus, host_status: impl Into<String>, epoch: i64) -> HandleSnapshot { HandleSnapshot { reference, phase: phase(status), host_status: host_status.into(), revision: epoch * 2 + i64::from(phase(status) != HandlePhase::Pending) } }
pub fn unsupported(kind: HandleKind, operation: &str) -> EvalHandleHostError { EvalHandleHostError::new(EvalHandleErrorCode::Unsupported, format!("{operation} is for agent handles, not {kind:?}")) }
