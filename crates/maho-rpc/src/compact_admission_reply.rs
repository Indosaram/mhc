//! senpi `modes/rpc/compact-admission-reply.ts`: bounded request-scoped reply routing,
//! ensuring async session replies keep their caller's admission route without a retained index.

use maho_core::engine_socket_flag::compact_updates_enabled;
use std::future::Future;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionReplyScope {
    pub id: Option<String>,
    pub admission_id: Option<String>,
    pub peer: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplyAdmission {
    /// Route the response under this admission ID.
    Route(String),
    /// Ownership cannot be proved; drop the reply rather than guessing a route.
    Drop,
    /// Matching legacy request that did not select an admission.
    Legacy,
}

tokio::task_local! {
    static ADMISSION_REQUEST_SCOPE: AdmissionReplyScope;
}

/// Bounded reply identity, not the command body or a retained request index, crosses an isolate.
pub fn capture_admission_reply() -> Option<AdmissionReplyScope> {
    ADMISSION_REQUEST_SCOPE.try_with(Clone::clone).ok()
}

pub async fn with_admission_reply<F, T>(scope: Option<AdmissionReplyScope>, action: F) -> T
where
    F: Future<Output = T>,
{
    match scope {
        None => action.await,
        Some(scope) => ADMISSION_REQUEST_SCOPE.scope(scope, action).await,
    }
}

pub fn with_admission_reply_sync<T>(
    scope: Option<AdmissionReplyScope>,
    action: impl FnOnce() -> T,
) -> T {
    match scope {
        None => action(),
        Some(scope) => ADMISSION_REQUEST_SCOPE.sync_scope(scope, action),
    }
}

/// Request scope, not a retained request index: async session replies keep their caller's route.
pub async fn with_admission_request<F, T>(
    id: Option<String>,
    admission_id: Option<String>,
    peer: Option<String>,
    action: F,
) -> T
where
    F: Future<Output = T>,
{
    if !compact_updates_enabled() {
        return action.await;
    }
    let scope = AdmissionReplyScope {
        id,
        admission_id,
        peer,
    };
    ADMISSION_REQUEST_SCOPE.scope(scope, action).await
}

pub fn with_admission_request_sync<T>(
    id: Option<String>,
    admission_id: Option<String>,
    peer: Option<String>,
    action: impl FnOnce() -> T,
) -> T {
    if !compact_updates_enabled() {
        return action();
    }
    let scope = AdmissionReplyScope {
        id,
        admission_id,
        peer,
    };
    ADMISSION_REQUEST_SCOPE.sync_scope(scope, action)
}

/// Resolves how a `response` with `id` on `peer` should be tagged given the session's `routes`.
pub fn reply_admission(id: &str, peer: Option<&str>, routes: &[String]) -> ReplyAdmission {
    let Ok(scope) = ADMISSION_REQUEST_SCOPE.try_with(Clone::clone) else {
        return ReplyAdmission::Drop;
    };
    // No matching request scope means ownership cannot be proved. Drop, never guess routes[0];
    // Legacy is reserved for a matching legacy request that did not select an admission.
    if scope.id.as_deref() != Some(id) || scope.peer.as_deref() != peer {
        return ReplyAdmission::Drop;
    }
    let Some(admission_id) = scope.admission_id else {
        return ReplyAdmission::Legacy;
    };
    match routes.iter().find(|route| **route == admission_id) {
        Some(route) => ReplyAdmission::Route(route.clone()),
        None => ReplyAdmission::Drop,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_matching_route_legacy_and_mismatched_drops() {
        let routes = vec!["adm-1".to_owned(), "adm-2".to_owned()];
        assert_eq!(
            reply_admission("req-1", Some("p1"), &routes),
            ReplyAdmission::Drop
        );

        with_admission_reply_sync(
            Some(AdmissionReplyScope {
                id: Some("req-1".into()),
                admission_id: Some("adm-2".into()),
                peer: Some("p1".into()),
            }),
            || {
                assert_eq!(
                    reply_admission("req-1", Some("p1"), &routes),
                    ReplyAdmission::Route("adm-2".into())
                );
                assert_eq!(
                    reply_admission("req-2", Some("p1"), &routes),
                    ReplyAdmission::Drop
                );
                assert_eq!(
                    reply_admission("req-1", Some("p2"), &routes),
                    ReplyAdmission::Drop
                );
                assert_eq!(
                    reply_admission("req-1", Some("p1"), &["adm-1".into()]),
                    ReplyAdmission::Drop
                );
            },
        );

        with_admission_reply_sync(
            Some(AdmissionReplyScope {
                id: Some("req-1".into()),
                admission_id: None,
                peer: Some("p1".into()),
            }),
            || {
                assert_eq!(
                    reply_admission("req-1", Some("p1"), &routes),
                    ReplyAdmission::Legacy
                );
            },
        );
    }

    #[tokio::test]
    async fn capture_and_replay_crosses_async_boundary() {
        let captured = with_admission_reply(
            Some(AdmissionReplyScope {
                id: Some("r1".into()),
                admission_id: Some("a1".into()),
                peer: Some("peer".into()),
            }),
            async { capture_admission_reply() },
        )
        .await;
        assert!(captured.is_some());
        let resolved = with_admission_reply(captured, async {
            reply_admission("r1", Some("peer"), &["a1".into()])
        })
        .await;
        assert_eq!(resolved, ReplyAdmission::Route("a1".into()));
    }
}
