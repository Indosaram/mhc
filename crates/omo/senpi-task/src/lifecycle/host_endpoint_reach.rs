//! Reaching the endpoint a record names (`lifecycle/host-endpoint-reach.ts`).

use super::context::{LifecycleContext, reconciliation_active};
use super::host_session::{HostEndpointEnsure, HostEndpointNotice};
use crate::state::{HostSessionIdentity, SuspensionReason};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordedHostVerdict {
    Alive,
    HostUnreachable,
    HostIncompatible,
    OwnHostUnreachable,
}

impl RecordedHostVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Alive => "alive",
            Self::HostUnreachable => "host_unreachable",
            Self::HostIncompatible => "host_incompatible",
            Self::OwnHostUnreachable => "own_host_unreachable",
        }
    }
}

/// Reach the endpoint a record names - that socket and no other. A probe that answers (any host
/// generation) is enough. A silent endpoint is ensured, unless it is the endpoint this session runs
/// behind: that one is only probed. A pass that went stale at any step answers `host_unreachable`,
/// so a late answer never authorizes a claim or a respawn.
pub fn reach_recorded_host(
    context: &LifecycleContext,
    host_session: &HostSessionIdentity,
) -> RecordedHostVerdict {
    if !reconciliation_active(context) {
        return RecordedHostVerdict::HostUnreachable;
    }
    let alive = context.host_session_probe.daemon_alive(host_session);
    if !reconciliation_active(context) {
        return RecordedHostVerdict::HostUnreachable;
    }
    if alive {
        return RecordedHostVerdict::Alive;
    }
    let endpoint = &context.host_endpoint;
    if endpoint.is_own(&host_session.socket) {
        endpoint.notice(HostEndpointNotice::OwnHostUnreachable, &host_session.socket);
        return RecordedHostVerdict::OwnHostUnreachable;
    }
    let ensured = endpoint.ensure(&host_session.socket);
    if !reconciliation_active(context) {
        return RecordedHostVerdict::HostUnreachable;
    }
    if ensured == HostEndpointEnsure::Incompatible {
        endpoint.notice(HostEndpointNotice::HostIncompatible, &host_session.socket);
        return RecordedHostVerdict::HostIncompatible;
    }
    if ensured != HostEndpointEnsure::Ensured {
        return RecordedHostVerdict::HostUnreachable;
    }
    context
        .host_session_probe
        .refresh(Some(&host_session.socket));
    let reached = context.host_session_probe.daemon_alive(host_session);
    if reconciliation_active(context) && reached {
        RecordedHostVerdict::Alive
    } else {
        RecordedHostVerdict::HostUnreachable
    }
}

/// What task_output reports for a record a verdict left parked (`None` for `alive`).
pub fn parked_reason(verdict: RecordedHostVerdict) -> Option<SuspensionReason> {
    match verdict {
        RecordedHostVerdict::Alive => None,
        RecordedHostVerdict::HostUnreachable => Some(SuspensionReason::DaemonUnavailable),
        RecordedHostVerdict::HostIncompatible => Some(SuspensionReason::HostIncompatible),
        RecordedHostVerdict::OwnHostUnreachable => Some(SuspensionReason::OwnHostUnreachable),
    }
}
