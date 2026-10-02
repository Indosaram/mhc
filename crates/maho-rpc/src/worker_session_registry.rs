use std::collections::HashMap;
use crate::{session_registry::RpcSessionState,session_path_reservations::{SessionPathReservations,SessionWriteGrant,LiveWorkerPaths}};
#[derive(Default)]pub struct WorkerOwnership{pub states:HashMap<String,RpcSessionState>,pub reservations:SessionPathReservations}
impl WorkerOwnership{
    pub fn reserve(&mut self,handle:&str,path:&str,live:Option<&LiveWorkerPaths<'_>>)->SessionWriteGrant{
        let Some(state)=self.states.get(handle)else{return SessionWriteGrant::Conflict;};
        if self.reservations.owner(path)==Some(handle){return SessionWriteGrant::Granted;}
        if !matches!(state,RpcSessionState::Opening|RpcSessionState::Open){return SessionWriteGrant::Conflict;}
        self.reservations.reserve(handle,path,if *state==RpcSessionState::Open{live}else{None})
    }
    pub fn reconcile(&mut self,handle:&str,live:&LiveWorkerPaths<'_>){if self.states.get(handle)==Some(&RpcSessionState::Open){self.reservations.reconcile(handle,live);}}
    pub fn exited(&mut self,handle:&str){self.states.remove(handle);self.reservations.release_all(handle);}
}
#[cfg(test)]mod tests{use super::*;#[test]fn quarantine_retains_existing_writer_claim_until_actual_exit(){let mut registry=WorkerOwnership::default();registry.states.insert("one".into(),RpcSessionState::Opening);assert_eq!(registry.reserve("one","/path",None),SessionWriteGrant::Granted);registry.states.insert("one".into(),RpcSessionState::Quarantined);registry.reconcile("one",&LiveWorkerPaths{live_paths:&[],session_path:None});assert_eq!(registry.reserve("one","/path",None),SessionWriteGrant::Granted);assert_eq!(registry.reserve("one","/other",None),SessionWriteGrant::Conflict);registry.states.insert("two".into(),RpcSessionState::Open);assert_eq!(registry.reserve("two","/path",None),SessionWriteGrant::Conflict);registry.exited("one");assert_eq!(registry.reserve("two","/path",None),SessionWriteGrant::Granted);}#[test]fn opening_cannot_release_claims_from_incomplete_snapshot(){let mut registry=WorkerOwnership::default();registry.states.insert("one".into(),RpcSessionState::Opening);registry.reserve("one","/old",None);let live=LiveWorkerPaths{live_paths:&[],session_path:Some("/new")};registry.reconcile("one",&live);assert_eq!(registry.reservations.count("one"),1);registry.states.insert("one".into(),RpcSessionState::Open);registry.reconcile("one",&live);assert_eq!(registry.reservations.count("one"),0);}}
