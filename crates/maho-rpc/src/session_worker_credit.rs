use crate::{session_worker_signals::WorkerSignal,session_worker_protocol::SESSION_WORKER_LIMITS};
#[derive(Debug,thiserror::Error,PartialEq,Eq)]pub enum WorkerCreditError{
    #[error("{0}")]Denied(String),
    #[error("session_worker_credit_timeout")]Timeout,
    #[error("session_path_in_use")]PathInUse,
    #[error("session_reservation_limit")]ReservationLimit,
}
pub fn exchange(send:impl FnOnce(&WorkerSignal),denied_error:Option<&str>,signal:Option<WorkerSignal>)->Result<(),WorkerCreditError>{let signal=signal.unwrap_or_default();send(&signal);let result=signal.wait(std::time::Duration::from_millis(SESSION_WORKER_LIMITS.control_ms));match result{1=>Ok(()),2=>Err(WorkerCreditError::Denied(denied_error.unwrap_or("session_worker_output_denied").into())),_=>Err(WorkerCreditError::Timeout)}}
pub fn reserve_write(send:impl FnOnce(&WorkerSignal))->Result<(),WorkerCreditError>{let signal=WorkerSignal::default();send(&signal);match signal.wait(std::time::Duration::from_millis(SESSION_WORKER_LIMITS.control_ms)){1=>Ok(()),2=>Err(WorkerCreditError::PathInUse),3=>Err(WorkerCreditError::ReservationLimit),_=>Err(WorkerCreditError::Timeout)}}
#[cfg(test)]mod tests{use super::*;use crate::session_worker_protocol::SessionWriteGrant;#[test]fn generic_exchange_denies_conflict_and_treats_limit_as_timeout(){assert_eq!(exchange(|signal|signal.acknowledge(false),None,None),Err(WorkerCreditError::Denied("session_worker_output_denied".into())));assert_eq!(exchange(|signal|signal.acknowledge_grant(SessionWriteGrant::Limit),None,None),Err(WorkerCreditError::Timeout));}#[test]fn reservation_denials_are_domain_failures(){assert_eq!(reserve_write(|signal|signal.acknowledge_grant(SessionWriteGrant::Limit)),Err(WorkerCreditError::ReservationLimit));assert_eq!(reserve_write(|signal|signal.acknowledge(false)),Err(WorkerCreditError::PathInUse));assert_eq!(reserve_write(|signal|signal.acknowledge(true)),Ok(()));}}
