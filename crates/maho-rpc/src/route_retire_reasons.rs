//! Port of senpi `packages/coding-agent/src/modes/rpc/route-retire-reasons.ts`.
//!
//! RULING-119 (#5274): the engine never retires a delivering route silently. Every retire is
//! either the direct result of a client action whose reply the client sees, or announced to that
//! client by a typed record before the route stops delivering.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteRetireKind {
    ClientAction,
    Announced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientActionRetire {
    ReopenSuperseded,
    ClientDetach,
    MirrorUnsubscribed,
}

impl ClientActionRetire {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReopenSuperseded => "reopen_superseded",
            Self::ClientDetach => "client_detach",
            Self::MirrorUnsubscribed => "mirror_unsubscribed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnouncedRetire {
    OpenEnded,
}

impl AnnouncedRetire {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenEnded => "open_ended",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteRetireReason {
    ReopenSuperseded,
    ClientDetach,
    MirrorUnsubscribed,
    OpenEnded,
}

impl RouteRetireReason {
    pub const fn kind(self) -> RouteRetireKind {
        match self {
            Self::ReopenSuperseded | Self::ClientDetach | Self::MirrorUnsubscribed => {
                RouteRetireKind::ClientAction
            }
            Self::OpenEnded => RouteRetireKind::Announced,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReopenSuperseded => "reopen_superseded",
            Self::ClientDetach => "client_detach",
            Self::MirrorUnsubscribed => "mirror_unsubscribed",
            Self::OpenEnded => "open_ended",
        }
    }
}

/// Proof that the typed record ending an admission was enqueued; only `RouteAnnouncement::emit`
/// makes one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteAnnouncement {
    reason: AnnouncedRetire,
}

impl RouteAnnouncement {
    pub fn emit(reason: AnnouncedRetire, emit: impl FnOnce()) -> Self {
        emit();
        Self { reason }
    }

    pub const fn reason(self) -> AnnouncedRetire {
        self.reason
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteRetirement {
    ClientAction(ClientActionRetire),
    Announced(RouteAnnouncement),
}

impl RouteRetirement {
    pub const fn reason(self) -> RouteRetireReason {
        match self {
            Self::ClientAction(ClientActionRetire::ReopenSuperseded) => {
                RouteRetireReason::ReopenSuperseded
            }
            Self::ClientAction(ClientActionRetire::ClientDetach) => RouteRetireReason::ClientDetach,
            Self::ClientAction(ClientActionRetire::MirrorUnsubscribed) => {
                RouteRetireReason::MirrorUnsubscribed
            }
            Self::Announced(ann) => match ann.reason() {
                AnnouncedRetire::OpenEnded => RouteRetireReason::OpenEnded,
            },
        }
    }
}

impl From<ClientActionRetire> for RouteRetirement {
    fn from(value: ClientActionRetire) -> Self {
        Self::ClientAction(value)
    }
}

impl From<RouteAnnouncement> for RouteRetirement {
    fn from(value: RouteAnnouncement) -> Self {
        Self::Announced(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announcement_emits_before_returning_proof() {
        let mut emitted = false;
        let proof = RouteAnnouncement::emit(AnnouncedRetire::OpenEnded, || emitted = true);
        assert!(emitted);
        let ret: RouteRetirement = proof.into();
        assert_eq!(ret.reason(), RouteRetireReason::OpenEnded);
        assert_eq!(ret.reason().kind(), RouteRetireKind::Announced);
    }
}
