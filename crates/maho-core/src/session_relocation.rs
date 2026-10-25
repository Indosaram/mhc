//! Host-mediated session relocation protocol.
//!
//! Port of senpi `packages/coding-agent/src/core/session-relocation.ts`.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// One computer the host offers; the agent names it by `name`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelocationTarget {
    pub name: String,
    pub online: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capacity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub about_to_sleep: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelocationRequest {
    pub target: String,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelocationUnavailableReason {
    NotOffered,
    NotReleasable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelocationUnavailable {
    pub status: String,
    pub reason: RelocationUnavailableReason,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelocationRefused {
    pub status: String,
    pub reason: String,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RelocationTargetsResult {
    #[serde(rename = "listed")]
    Listed { targets: Vec<RelocationTarget> },
    #[serde(rename = "unavailable")]
    Unavailable {
        reason: RelocationUnavailableReason,
        detail: String,
    },
    #[serde(rename = "refused")]
    Refused { reason: String, detail: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RelocationRequestResult {
    #[serde(rename = "scheduled")]
    Scheduled { target: String, move_id: String },
    #[serde(rename = "unavailable")]
    Unavailable {
        reason: RelocationUnavailableReason,
        detail: String,
    },
    #[serde(rename = "refused")]
    Refused { reason: String, detail: String },
}

pub fn relocation_not_offered() -> RelocationUnavailable {
    RelocationUnavailable {
        status: "unavailable".into(),
        reason: RelocationUnavailableReason::NotOffered,
        detail: "no app connected to this session offers moving it to another computer".into(),
    }
}

pub fn relocation_not_offered_targets() -> RelocationTargetsResult {
    RelocationTargetsResult::Unavailable {
        reason: RelocationUnavailableReason::NotOffered,
        detail: "no app connected to this session offers moving it to another computer".into(),
    }
}

pub fn relocation_not_offered_request() -> RelocationRequestResult {
    RelocationRequestResult::Unavailable {
        reason: RelocationUnavailableReason::NotOffered,
        detail: "no app connected to this session offers moving it to another computer".into(),
    }
}

#[async_trait]
pub trait SessionRelocationHost: Send + Sync {
    /// Whether a host offers relocation for this session right now.
    fn offered(&self) -> bool;

    /// Available relocation targets.
    async fn targets(&self) -> RelocationTargetsResult;

    /// Request session relocation to `request.target`.
    async fn request(&self, request: RelocationRequest) -> RelocationRequestResult;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockHost {
        offered: bool,
    }

    #[async_trait]
    impl SessionRelocationHost for MockHost {
        fn offered(&self) -> bool {
            self.offered
        }

        async fn targets(&self) -> RelocationTargetsResult {
            if self.offered {
                RelocationTargetsResult::Listed {
                    targets: vec![RelocationTarget {
                        name: "worker-box".into(),
                        online: true,
                        capacity: Some("high".into()),
                        about_to_sleep: None,
                    }],
                }
            } else {
                relocation_not_offered_targets()
            }
        }

        async fn request(&self, request: RelocationRequest) -> RelocationRequestResult {
            if self.offered {
                RelocationRequestResult::Scheduled {
                    target: request.target,
                    move_id: "m-123".into(),
                }
            } else {
                relocation_not_offered_request()
            }
        }
    }

    #[tokio::test]
    async fn mock_host_operates_when_offered() {
        let host = MockHost { offered: true };
        assert!(host.offered());
        let targets = host.targets().await;
        assert!(matches!(targets, RelocationTargetsResult::Listed { .. }));
        let req = host
            .request(RelocationRequest {
                target: "worker-box".into(),
                reason: "load".into(),
            })
            .await;
        assert!(matches!(req, RelocationRequestResult::Scheduled { .. }));
    }

    #[tokio::test]
    async fn mock_host_returns_not_offered_when_disabled() {
        let host = MockHost { offered: false };
        assert!(!host.offered());
        let targets = host.targets().await;
        assert!(matches!(targets, RelocationTargetsResult::Unavailable { .. }));
    }
}
