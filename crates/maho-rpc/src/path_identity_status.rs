//! Port of senpi `packages/coding-agent/src/modes/rpc/path-identity-status.ts`.
//!
//! What `mhc host status` says about the path identity resolver: `not_started` until a worker
//! has published its first mount snapshot, `ready` after it, or `unavailable` with why the
//! worker could not start.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PATH_IDENTITY_STATES: [&str; 3] = ["not_started", "ready", "unavailable"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathIdentityResolverState {
    NotStarted,
    Ready,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathIdentityResolverStatus {
    pub state: PathIdentityResolverState,
    pub detail: Option<String>,
}

/// Snapshot view of a path identity resolver for status reporting.
pub trait PathIdentityResolverView {
    fn unavailable_detail(&self) -> Option<&str>;
    fn has_snapshot(&self) -> bool;
}

pub fn path_identity_resolver_status(
    resolver: &dyn PathIdentityResolverView,
) -> PathIdentityResolverStatus {
    if let Some(detail) = resolver.unavailable_detail() {
        return PathIdentityResolverStatus {
            state: PathIdentityResolverState::Unavailable,
            detail: Some(detail.to_owned()),
        };
    }
    PathIdentityResolverStatus {
        state: if resolver.has_snapshot() {
            PathIdentityResolverState::Ready
        } else {
            PathIdentityResolverState::NotStarted
        },
        detail: None,
    }
}

pub fn parse_path_identity_resolver_status(value: &Value) -> Option<PathIdentityResolverStatus> {
    let obj = value.as_object()?;
    let state = match obj.get("state")?.as_str()? {
        "not_started" => PathIdentityResolverState::NotStarted,
        "ready" => PathIdentityResolverState::Ready,
        "unavailable" => PathIdentityResolverState::Unavailable,
        _ => return None,
    };
    let detail = obj
        .get("detail")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    Some(PathIdentityResolverStatus { state, detail })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct MockResolver {
        unavailable: Option<String>,
        snapshot: bool,
    }

    impl PathIdentityResolverView for MockResolver {
        fn unavailable_detail(&self) -> Option<&str> {
            self.unavailable.as_deref()
        }
        fn has_snapshot(&self) -> bool {
            self.snapshot
        }
    }

    #[test]
    fn maps_states_and_parses_wire_shape() {
        let s1 = path_identity_resolver_status(&MockResolver {
            unavailable: None,
            snapshot: false,
        });
        assert_eq!(s1.state, PathIdentityResolverState::NotStarted);
        assert_eq!(s1.detail, None);

        let s2 = path_identity_resolver_status(&MockResolver {
            unavailable: None,
            snapshot: true,
        });
        assert_eq!(s2.state, PathIdentityResolverState::Ready);

        let s3 = path_identity_resolver_status(&MockResolver {
            unavailable: Some("spawn failed".to_owned()),
            snapshot: false,
        });
        assert_eq!(s3.state, PathIdentityResolverState::Unavailable);
        assert_eq!(s3.detail.as_deref(), Some("spawn failed"));

        let parsed = parse_path_identity_resolver_status(&json!({"state":"ready","detail":null}))
            .expect("valid status");
        assert_eq!(parsed, s2);
        assert!(parse_path_identity_resolver_status(&json!({"state":"bogus"})).is_none());
    }
}
