use std::collections::HashMap;
pub const SESSION_WORKER_RESERVATIONS:usize = 64;
/// Wire code each denial is reported with; distinct so clients can retry only the retryable one.
#[derive(Debug,Clone,Copy,PartialEq,Eq)]
pub struct ReservationDenialCodes{pub conflict:&'static str,pub limit:&'static str}
pub const RESERVATION_DENIAL_CODES:ReservationDenialCodes=ReservationDenialCodes{conflict:"session_path_in_use",limit:"session_reservation_limit"};
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum SessionWriteGrant { Granted, Conflict, Limit }
impl SessionWriteGrant {
    pub const fn denial_code(self) -> Option<&'static str> {
        match self { Self::Granted => None,Self::Conflict => Some("session_path_in_use"),Self::Limit => Some("session_reservation_limit") }
    }
}
pub struct LiveWorkerPaths<'a> { pub live_paths:&'a [String],pub session_path:Option<&'a str> }
#[derive(Default)]
pub struct SessionPathReservations { owners:HashMap<String,String> }
impl SessionPathReservations {
    pub fn owner(&self,path:&str) -> Option<&str> { self.owners.get(path).map(String::as_str) }
    pub fn count(&self,handle:&str) -> usize { self.owners.values().filter(|owner| *owner == handle).count() }
    pub fn reserve(&mut self,handle:&str,path:&str,live:Option<&LiveWorkerPaths<'_>>) -> SessionWriteGrant {
        if let Some(owner) = self.owner(path) { return if owner == handle { SessionWriteGrant::Granted } else { SessionWriteGrant::Conflict }; }
        if self.count(handle) >= SESSION_WORKER_RESERVATIONS {
            let Some(live) = live else { return SessionWriteGrant::Limit; };
            self.reconcile(handle,live);
            if self.count(handle) >= SESSION_WORKER_RESERVATIONS { return SessionWriteGrant::Limit; }
        }
        self.owners.insert(path.into(),handle.into());
        SessionWriteGrant::Granted
    }
    pub fn reconcile(&mut self,handle:&str,live:&LiveWorkerPaths<'_>) {
        self.owners.retain(|path,owner| owner != handle || Some(path.as_str()) == live.session_path || live.live_paths.contains(path));
    }
    pub fn release_all(&mut self,handle:&str) { self.owners.retain(|_,owner| owner != handle); }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn reservation_denial_codes_match_the_enum(){assert_eq!(SessionWriteGrant::Conflict.denial_code(),Some(RESERVATION_DENIAL_CODES.conflict));assert_eq!(SessionWriteGrant::Limit.denial_code(),Some(RESERVATION_DENIAL_CODES.limit));assert_eq!(SessionWriteGrant::Granted.denial_code(),None);}
    #[test] fn reservation_denials_distinguish_budget_from_ownership() {
        let mut reservations = SessionPathReservations::default();
        let live = (0..SESSION_WORKER_RESERVATIONS).map(|i| format!("/live/session-{i}.jsonl")).collect::<Vec<_>>();
        for path in &live { assert_eq!(reservations.reserve("rpc-1",path,None),SessionWriteGrant::Granted); }
        assert_eq!(reservations.reserve("rpc-1","/overflow",Some(&LiveWorkerPaths { live_paths:&live,session_path:None })).denial_code(),Some("session_reservation_limit"));
        assert_eq!(reservations.reserve("rpc-2",&live[0],None).denial_code(),Some("session_path_in_use"));
        assert_eq!(reservations.count("rpc-1"),64);
    }
    #[test] fn superseded_paths_release_budget_at_cap() {
        let mut reservations = SessionPathReservations::default();
        for i in 0..64 { reservations.reserve("rpc-1",&format!("/live/{i}"),None); }
        let live = LiveWorkerPaths { live_paths:&[],session_path:Some("/live/0") };
        assert_eq!(reservations.reserve("rpc-1","/replacement",Some(&live)),SessionWriteGrant::Granted);
        assert_eq!(reservations.count("rpc-1"),2);
    }
    #[test] fn owner_can_reacquire_at_cap() {
        let mut reservations = SessionPathReservations::default();
        for i in 0..64 { reservations.reserve("rpc-1",&format!("/live/{i}"),None); }
        assert_eq!(reservations.reserve("rpc-1","/live/0",None),SessionWriteGrant::Granted);
    }
    #[test] fn reconcile_and_release_never_remove_other_owners() {
        let mut reservations = SessionPathReservations::default();
        reservations.reserve("one","/one",None); reservations.reserve("two","/two",None);
        reservations.reconcile("one",&LiveWorkerPaths { live_paths:&[],session_path:None });
        assert_eq!(reservations.owner("/two"),Some("two"));
        reservations.release_all("one");
        assert_eq!(reservations.owner("/two"),Some("two"));
    }
}
