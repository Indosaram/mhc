//! Port of senpi `packages/coding-agent/src/modes/rpc/worker-client-capabilities.ts`.
//!
//! Workers acknowledge a host-approved declaration; they never choose an actual socket's format.

use crate::compact_negotiation::{CompactSelection, RoleDeclarationVerdict};
use crate::connection_roles::role_connection;
use crate::session_worker_protocol::SessionWriteGrant;
use crate::session_worker_signals::WorkerSignal;
use maho_core::engine_socket_flag::compact_updates_enabled;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerCapabilityVerdict {
    Granted,
    Refused,
    Stale,
}

pub trait WorkerCapabilitiesHost {
    fn is_role_stale(&self, connection: &str, incarnation: Option<u64>) -> bool;
    fn compact_selection(&self, connection: &str) -> Option<CompactSelection>;
    fn declare_role(
        &self,
        connection: &str,
        capabilities: &[String],
        incarnation: Option<u64>,
    ) -> RoleDeclarationVerdict;
    fn set_sole_capabilities(&mut self, capabilities: Vec<String>);
    fn set_client_info_capabilities(&self, connection: Option<&str>, capabilities: &[String]);
}

pub fn acknowledge_capability_signal(
    signal: Option<&WorkerSignal>,
    verdict: WorkerCapabilityVerdict,
) {
    let Some(signal) = signal else {
        return;
    };
    match verdict {
        WorkerCapabilityVerdict::Granted => signal.acknowledge_grant(SessionWriteGrant::Granted),
        WorkerCapabilityVerdict::Refused => signal.acknowledge_grant(SessionWriteGrant::Conflict),
        WorkerCapabilityVerdict::Stale => signal.acknowledge_grant(SessionWriteGrant::Limit),
    }
}

pub fn worker_capabilities<H: WorkerCapabilitiesHost + ?Sized>(
    host: &mut H,
    connection: Option<&str>,
    capabilities: &[String],
    incarnation: Option<u64>,
    signal: Option<&WorkerSignal>,
) -> WorkerCapabilityVerdict {
    let role_conn = role_connection(connection);
    // omo-relay-daemon#307 (v5 review H1): a declaration from a retired incarnation is answered
    // `stale` BEFORE the compact check, which would otherwise answer conflict and end the worker.
    if host.is_role_stale(role_conn, incarnation) {
        acknowledge_capability_signal(signal, WorkerCapabilityVerdict::Stale);
        return WorkerCapabilityVerdict::Stale;
    }
    let current = match connection {
        Some(conn) if compact_updates_enabled() => host.compact_selection(conn),
        _ => None,
    };
    if let Some(current) = current
        && (current.refused
            || capabilities.len() != current.capabilities.len()
            || !capabilities
                .iter()
                .all(|cap| current.capabilities.contains(cap)))
    {
        acknowledge_capability_signal(signal, WorkerCapabilityVerdict::Refused);
        return WorkerCapabilityVerdict::Refused;
    }
    match host.declare_role(role_conn, capabilities, incarnation) {
        RoleDeclarationVerdict::Stale => {
            acknowledge_capability_signal(signal, WorkerCapabilityVerdict::Stale);
            return WorkerCapabilityVerdict::Stale;
        }
        RoleDeclarationVerdict::Fixed => {
            acknowledge_capability_signal(signal, WorkerCapabilityVerdict::Refused);
            return WorkerCapabilityVerdict::Refused;
        }
        RoleDeclarationVerdict::Declared => {}
    }
    if connection.is_none() {
        host.set_sole_capabilities(capabilities.to_vec());
    }
    host.set_client_info_capabilities(connection, capabilities);
    acknowledge_capability_signal(signal, WorkerCapabilityVerdict::Granted);
    WorkerCapabilityVerdict::Granted
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct FakeHost {
        stale: bool,
        compact: Option<CompactSelection>,
        role_verdict: RoleDeclarationVerdict,
        sole: Vec<String>,
        applied: Mutex<Vec<(Option<String>, Vec<String>)>>,
    }

    impl WorkerCapabilitiesHost for FakeHost {
        fn is_role_stale(&self, _: &str, _: Option<u64>) -> bool {
            self.stale
        }
        fn compact_selection(&self, _: &str) -> Option<CompactSelection> {
            self.compact.clone()
        }
        fn declare_role(&self, _: &str, _: &[String], _: Option<u64>) -> RoleDeclarationVerdict {
            self.role_verdict
        }
        fn set_sole_capabilities(&mut self, capabilities: Vec<String>) {
            self.sole = capabilities;
        }
        fn set_client_info_capabilities(&self, connection: Option<&str>, capabilities: &[String]) {
            self.applied
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((connection.map(str::to_owned), capabilities.to_vec()));
        }
    }

    #[test]
    fn stale_incarnation_short_circuits_before_compact_check() {
        let mut host = FakeHost {
            stale: true,
            compact: Some(CompactSelection {
                refused: true,
                ..Default::default()
            }),
            role_verdict: RoleDeclarationVerdict::Declared,
            sole: Vec::new(),
            applied: Mutex::new(Vec::new()),
        };
        let sig = WorkerSignal::default();
        let verdict = worker_capabilities(&mut host, Some("c1"), &[], Some(1), Some(&sig));
        assert_eq!(verdict, WorkerCapabilityVerdict::Stale);
        assert_eq!(sig.wait(std::time::Duration::ZERO), 3);
        assert!(
            host.applied
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );
    }

    #[test]
    fn fixed_role_refuses_and_valid_sole_applies() {
        let mut host = FakeHost {
            stale: false,
            compact: None,
            role_verdict: RoleDeclarationVerdict::Fixed,
            sole: Vec::new(),
            applied: Mutex::new(Vec::new()),
        };
        assert_eq!(
            worker_capabilities(&mut host, None, &["question".into()], None, None),
            WorkerCapabilityVerdict::Refused
        );
        host.role_verdict = RoleDeclarationVerdict::Declared;
        assert_eq!(
            worker_capabilities(&mut host, None, &["question".into()], None, None),
            WorkerCapabilityVerdict::Granted
        );
        assert_eq!(host.sole, vec!["question"]);
    }
}
