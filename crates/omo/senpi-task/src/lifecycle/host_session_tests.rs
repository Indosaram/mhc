//! Host-session record helpers (`lifecycle/host-session.ts` isHostSessionRecord / hostSessionResumePath).

use crate::state::{HostSessionIdentity, RunnerKind};
use crate::test_support::base_record;

use super::{host_session_identity, host_session_resume_path, is_host_session_record};

fn identity(session_path: &str) -> HostSessionIdentity {
    HostSessionIdentity {
        socket: "/tmp/host.sock".to_string(),
        routing_id: "rpc-1".to_string(),
        session_path: session_path.to_string(),
        instance_id: "inst-1".to_string(),
        daemon_pid: Some(4242),
    }
}

#[test]
fn host_session_record_is_host_session_kind_with_an_identity() {
    let mut record = base_record("st_00000001", "parent-1");
    assert!(!is_host_session_record(&record));
    assert!(host_session_resume_path(&record).is_none());

    record.runner_kind = Some(RunnerKind::HostSession);
    record.host_session = None;
    assert!(!is_host_session_record(&record));
    assert!(host_session_resume_path(&record).is_none());

    record.runner_kind = Some(RunnerKind::ChildProcess);
    record.host_session = Some(identity("/tmp/child.jsonl"));
    assert!(!is_host_session_record(&record));
    assert!(host_session_resume_path(&record).is_none());
}

#[test]
fn host_session_resume_path_is_the_recorded_session_path() {
    let mut record = base_record("st_00000002", "parent-1");
    record.runner_kind = Some(RunnerKind::HostSession);
    record.host_session = Some(identity("/tmp/host.jsonl"));

    assert!(is_host_session_record(&record));
    assert_eq!(
        host_session_identity(&record).map(|host| host.session_path.as_str()),
        Some("/tmp/host.jsonl")
    );
    assert_eq!(host_session_resume_path(&record), Some("/tmp/host.jsonl"));
}
