use crate::{session_worker_signals::WorkerSignal,session_worker_protocol::SESSION_WORKER_LIMITS};
#[derive(Debug,thiserror::Error,PartialEq,Eq)]pub enum WorkerCreditError{
    #[error("{0}")]Denied(String),
    #[error("session_worker_credit_timeout")]Timeout,
    #[error("session_path_in_use")]PathInUse,
    #[error("session_reservation_limit")]ReservationLimit,
}
pub fn exchange(send:impl FnOnce(&WorkerSignal),denied_error:Option<&str>,signal:Option<WorkerSignal>)->Result<(),WorkerCreditError>{let signal=signal.unwrap_or_default();send(&signal);let result=signal.wait(std::time::Duration::from_millis(SESSION_WORKER_LIMITS.control_ms));match result{1=>Ok(()),2=>Err(WorkerCreditError::Denied(denied_error.unwrap_or("session_worker_output_denied").into())),_=>Err(WorkerCreditError::Timeout)}}
pub fn reserve_write(send:impl FnOnce(&WorkerSignal))->Result<(),WorkerCreditError>{let signal=WorkerSignal::default();send(&signal);match signal.wait(std::time::Duration::from_millis(SESSION_WORKER_LIMITS.control_ms)){1=>Ok(()),2=>Err(WorkerCreditError::PathInUse),3=>Err(WorkerCreditError::ReservationLimit),_=>Err(WorkerCreditError::Timeout)}}
/// Synchronous host credit for a session worker (senpi `createWorkerCredit`): every exchange
/// blocks the worker on its own wait signal, so the host answers before the worker touches a
/// writer, publishes output, or resizes a display.
pub struct WorkerCredit { send:Box<dyn Fn(crate::session_worker_protocol::SessionWorkerToHost)+Send+Sync> }
impl WorkerCredit {
    /// Blocks the worker until the host answers; a denial fails the worker with `denied_error`.
    pub fn exchange(&self,build:impl Fn(&WorkerSignal)->crate::session_worker_protocol::SessionWorkerToHost,denied_error:Option<&str>,signal:Option<WorkerSignal>)->Result<(),WorkerCreditError>{
        let signal=signal.unwrap_or_default();(self.send)(build(&signal));
        match signal.wait(std::time::Duration::from_millis(SESSION_WORKER_LIMITS.control_ms)){1=>Ok(()),2=>Err(WorkerCreditError::Denied(denied_error.unwrap_or("session_worker_output_denied").into())),_=>Err(WorkerCreditError::Timeout)}
    }
    /// Asks the host for a session-write grant on `path`; both denials are session-level
    /// failures the caller reports, never worker-fatal (senpi `installWriteReservation`).
    pub fn reserve_write(&self,path:&str)->Result<(),WorkerCreditError>{
        let signal=WorkerSignal::default();(self.send)(crate::session_worker_protocol::SessionWorkerToHost::Reserve{path:path.into(),signal:signal.clone()});
        match signal.wait(std::time::Duration::from_millis(SESSION_WORKER_LIMITS.control_ms)){1=>Ok(()),2=>Err(WorkerCreditError::PathInUse),3=>Err(WorkerCreditError::ReservationLimit),_=>Err(WorkerCreditError::Timeout)}
    }
}
pub fn create_worker_credit(send:impl Fn(crate::session_worker_protocol::SessionWorkerToHost)+Send+Sync+'static)->WorkerCredit{WorkerCredit{send:Box::new(send)}}
#[cfg(test)]mod tests{use super::*;use crate::{session_worker_protocol::SessionWorkerToHost,session_worker_protocol::SessionWriteGrant};
fn credit_answering(answer:SessionWriteGrant)->WorkerCredit{create_worker_credit(move|message|{if let SessionWorkerToHost::Reserve{signal,..}=&message{signal.acknowledge_grant(answer);}})}
#[test]fn credit_exchange_builds_the_host_envelope_and_reports_denials(){let credit=create_worker_credit(|message|{if let SessionWorkerToHost::Reserve{signal,..}=&message{signal.acknowledge(false);}});assert_eq!(credit.exchange(|signal|SessionWorkerToHost::Reserve{path:"/s".into(),signal:signal.clone()},None,None),Err(WorkerCreditError::Denied("session_worker_output_denied".into())));assert_eq!(credit.exchange(|signal|SessionWorkerToHost::Reserve{path:"/s".into(),signal:signal.clone()},Some("custom"),None),Err(WorkerCreditError::Denied("custom".into())));}
#[test]fn credit_reserve_write_maps_grants_and_denials(){assert_eq!(credit_answering(SessionWriteGrant::Granted).reserve_write("/s"),Ok(()));assert_eq!(credit_answering(SessionWriteGrant::Conflict).reserve_write("/s"),Err(WorkerCreditError::PathInUse));assert_eq!(credit_answering(SessionWriteGrant::Limit).reserve_write("/s"),Err(WorkerCreditError::ReservationLimit));}
#[test]fn generic_exchange_denies_conflict_and_treats_limit_as_timeout(){assert_eq!(exchange(|signal|signal.acknowledge(false),None,None),Err(WorkerCreditError::Denied("session_worker_output_denied".into())));assert_eq!(exchange(|signal|signal.acknowledge_grant(SessionWriteGrant::Limit),None,None),Err(WorkerCreditError::Timeout));}#[test]fn reservation_denials_are_domain_failures(){assert_eq!(reserve_write(|signal|signal.acknowledge_grant(SessionWriteGrant::Limit)),Err(WorkerCreditError::ReservationLimit));assert_eq!(reserve_write(|signal|signal.acknowledge(false)),Err(WorkerCreditError::PathInUse));assert_eq!(reserve_write(|signal|signal.acknowledge(true)),Ok(()));}}
