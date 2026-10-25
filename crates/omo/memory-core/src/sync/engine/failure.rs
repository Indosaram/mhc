use super::error::EngineError;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncFailureKind {
    StrandedLock,
    AuthExpired,
    Offline,
    Unknown,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncFailure {
    pub kind: SyncFailureKind,
    pub paths: Vec<String>,
    pub reason: String,
    pub action: String,
}
pub const OFFLINE_SHOWN_AFTER_ATTEMPTS: u64 = 3;
pub fn is_failure_shown(kind: SyncFailureKind, attempts: u64) -> bool {
    kind != SyncFailureKind::Offline || attempts >= OFFLINE_SHOWN_AFTER_ATTEMPTS
}
pub fn describe_sync_failure(error: &EngineError) -> SyncFailure {
    let(kind,reason,action)=match error {
 EngineError::StrandedLock(paths)=>return SyncFailure{kind:SyncFailureKind::StrandedLock,paths:paths.clone(),reason:"A git lock file left in the memory repository blocks sync, and OmO did not remove it on its own.".into(),action:if paths.len()==1{format!("If no other session is working on this memory, run /memory unlock {}, or restart OmO.",paths[0])}else{format!("If no other session is working on this memory, run /memory unlock for each: {}; or restart OmO.",paths.join(", "))}},
 EngineError::Auth=> (SyncFailureKind::AuthExpired,"This device's OmO sign-in has expired or was refused, so memory sync cannot reach your account.","Sign in to OmO again."),
 EngineError::Http(e) if e.status==401 => (SyncFailureKind::AuthExpired,"This device's OmO sign-in has expired or was refused, so memory sync cannot reach your account.","Sign in to OmO again."),
 EngineError::Network=>(SyncFailureKind::Offline,"Memory sync cannot reach the OmO sync service.","Check your internet connection. Sync resumes on its own once the service is reachable."),
 EngineError::Http(e) if e.transient=>(SyncFailureKind::Offline,"Memory sync cannot reach the OmO sync service.","Check your internet connection. Sync resumes on its own once the service is reachable."),
 _=>(SyncFailureKind::Unknown,"Memory sync stopped on an unexpected error.","Sync retries on its own. If this stays, restart OmO; the error is in the memory log.")};
    SyncFailure {
        kind,
        paths: vec![],
        reason: reason.into(),
        action: action.into(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn offline_visibility_requires_three_attempts() {
        assert!(!is_failure_shown(SyncFailureKind::Offline, 2));
        assert!(is_failure_shown(SyncFailureKind::Offline, 3));
        assert!(is_failure_shown(SyncFailureKind::Unknown, 1));
    }
    #[test]
    fn status_never_leaks_raw_errors() {
        let a = describe_sync_failure(&EngineError::Other("password=secret".into()));
        assert!(!a.reason.contains("password"));
        assert_eq!(
            describe_sync_failure(&EngineError::Auth).kind,
            SyncFailureKind::AuthExpired
        );
        assert_eq!(
            describe_sync_failure(&EngineError::Network).kind,
            SyncFailureKind::Offline
        );
    }
}
