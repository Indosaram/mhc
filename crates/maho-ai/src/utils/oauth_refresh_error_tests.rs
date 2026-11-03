//! Port of the classification cases of senpi packages/ai/test/oauth-refresh-transient.test.ts
//! that do not need the credential store (`classifyOAuthRefreshFailure`,
//! `isOAuthRefreshUnavailableError`).

use super::*;

fn coded(code: &str) -> TransportCodeError {
    TransportCodeError {
        code: code.into(),
        message: "opaque".into(),
        cause: None,
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct Wrapper(
    &'static str,
    #[source] Option<Box<dyn StdError + Send + Sync>>,
);

#[test]
fn classifies_structured_failures() {
    for code in [
        "ConnectionRefused",
        "FailedToOpenSocket",
        "ECONNREFUSED",
        "ECONNRESET",
        "ENOTFOUND",
        "EAI_AGAIN",
        "ETIMEDOUT",
        "EPIPE",
        "UND_ERR_CONNECT_TIMEOUT",
        "UND_ERR_SOCKET",
    ] {
        assert_eq!(
            classify_oauth_refresh_failure(&coded(code)),
            OAuthRefreshFailureClass::Transient,
            "{code}"
        );
    }
    for code in [
        "ECONNREFUSED",
        "ECONNRESET",
        "ENOTFOUND",
        "EAI_AGAIN",
        "ETIMEDOUT",
        "UND_ERR_SOCKET",
    ] {
        let wrapped = Wrapper("fetch failed", Some(Box::new(coded(code))));
        assert_eq!(
            classify_oauth_refresh_failure(&wrapped),
            OAuthRefreshFailureClass::Transient,
            "{code}"
        );
    }
    let timeout = std::io::Error::new(std::io::ErrorKind::TimedOut, "opaque");
    assert_eq!(
        classify_oauth_refresh_failure(&timeout),
        OAuthRefreshFailureClass::Transient
    );
    for status in [408, 429, 500, 503, 599] {
        let error = OAuthTokenEndpointError::new("opaque", status);
        assert_eq!(
            classify_oauth_refresh_failure(&error),
            OAuthRefreshFailureClass::Transient,
            "{status}"
        );
    }
    for status in [400, 401, 403] {
        let error = OAuthTokenEndpointError::new("invalid_grant", status);
        assert_eq!(
            classify_oauth_refresh_failure(&error),
            OAuthRefreshFailureClass::Permanent,
            "{status}"
        );
    }
    assert_eq!(
        classify_oauth_refresh_failure(&Wrapper("connection refused 503", None)),
        OAuthRefreshFailureClass::Permanent
    );
    assert_eq!(
        classify_oauth_refresh_failure(&coded("UND_ERR_INVALID_ARG")),
        OAuthRefreshFailureClass::Permanent
    );
}

#[test]
fn reports_closed_cause_facts() {
    assert_eq!(
        oauth_refresh_failure_cause(&coded("ECONNREFUSED")).as_deref(),
        Some("connection_refused")
    );
    assert_eq!(
        oauth_refresh_failure_cause(&coded("ECONNRESET")).as_deref(),
        Some("econnreset")
    );
    assert_eq!(
        oauth_refresh_failure_cause(&OAuthTokenEndpointError::new("x", 503)).as_deref(),
        Some("http_503")
    );
}

#[test]
fn recognizes_the_unavailable_error_through_its_cause_chain() {
    let unavailable =
        OAuthRefreshUnavailableError::new("fixture", Some(Box::new(coded("ECONNRESET"))));
    assert_eq!(unavailable.models_error.code, ModelsErrorCode::OAuth);
    assert_eq!(unavailable.to_string(), "OAuth refresh failed for fixture");
    assert!(is_oauth_refresh_unavailable_error(&unavailable));
    let wrapped = Wrapper("pool exhausted", Some(Box::new(unavailable)));
    assert!(is_oauth_refresh_unavailable_error(&wrapped));
    assert!(!is_oauth_refresh_unavailable_error(&Wrapper(
        "OAuth refresh unavailable",
        None
    )));
}
