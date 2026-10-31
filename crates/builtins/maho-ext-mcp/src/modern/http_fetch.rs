//! Port of senpi mcp/modern/http-fetch.ts.
//!
//! The existing OAuth provider owns credentials. senpi builds a `FetchLike` that injects the bearer, strips
//! protocol headers, and runs the SDK `auth()` flow once on 401/insufficient_scope. The port's probe fetch
//! does the same over reqwest; modern request POSTs carry the same rules inside `ModernHttpTransport`.
pub use super::transport::{modern_credential_snapshot, within_origin_client};
use super::{
    era::{ProbeFetchError, ProbeHttpRequest, ProbeHttpResponse},
    parameter_headers::clear_mcp_parameter_headers,
};
use crate::{
    errors::{McpError, McpErrorKind},
    transport_sdk::McpTransportSpec,
};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

pub fn strip_protocol_headers(headers: &mut BTreeMap<String, String>) {
    headers.retain(|name, _| {
        ![
            "mcp-session-id",
            "mcp-name",
            "mcp-method",
            "mcp-protocol-version",
        ]
        .contains(&name.to_ascii_lowercase().as_str())
    });
    clear_mcp_parameter_headers(headers);
}

/// The probe fetch: configured headers minus protocol/parameter headers, bearer from the provider, one
/// authorized resend after a 401 (or 403 `insufficient_scope`) when `authorize_resend`.
pub async fn modern_http_fetch(
    spec: &McpTransportSpec,
    auth: Option<&Arc<crate::auth::oauth_refresh::McpRefreshManager>>,
    request: ProbeHttpRequest,
    timeout: Duration,
    authorize_resend: bool,
) -> Result<ProbeHttpResponse, ProbeFetchError> {
    let McpTransportSpec::Http { url, headers } = spec else {
        return Err(ProbeFetchError::Transport(
            "modern HTTP fetch needs an HTTP transport".into(),
        ));
    };
    let client =
        within_origin_client().map_err(|error| ProbeFetchError::Transport(error.to_string()))?;
    let send = |force: bool| {
        let client = client.clone();
        let request = request.clone();
        async move {
            let mut merged = headers.clone();
            strip_protocol_headers(&mut merged);
            merged.extend(request.headers.clone());
            merged.retain(|name, _| !name.eq_ignore_ascii_case("mcp-session-id"));
            let mut builder = client
                .post(url.clone())
                .timeout(timeout)
                .body(request.body.clone());
            if let Some(auth) = auth {
                let tokens = if force {
                    auth.force_refresh().await.map(Some)
                } else {
                    auth.ensure_fresh().await
                };
                let tokens = tokens.map_err(|error| {
                    // Same split as the legacy request path: only a terminal refresh failure means needs_auth.
                    if !crate::needs_auth::is_oauth_needs_auth_error(&error) {
                        return ProbeFetchError::Transport(error.to_string());
                    }
                    let mut failure = McpError::new(McpErrorKind::Auth, error.to_string());
                    failure.phase = Some("auth".into());
                    ProbeFetchError::NeedsAuth(Box::new(failure))
                })?;
                if let Some(tokens) = tokens {
                    builder = builder.bearer_auth(tokens.access_token);
                }
            }
            for (name, value) in &merged {
                builder = builder.header(name, value);
            }
            let response = builder.send().await.map_err(|error| {
                ProbeFetchError::Transport(crate::log::redact_urls_in_text(&error.to_string()))
            })?;
            let status = response.status().as_u16();
            let www = response
                .headers()
                .get("www-authenticate")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let content_type = response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let body = response.text().await.map_err(|error| {
                ProbeFetchError::Transport(crate::log::redact_urls_in_text(&error.to_string()))
            })?;
            Ok::<_, ProbeFetchError>((
                ProbeHttpResponse {
                    status,
                    content_type,
                    body,
                },
                www,
            ))
        }
    };
    let (response, www) = send(false).await?;
    if authorize_resend && auth.is_some() && (response.status == 401 || response.status == 403) {
        let insufficient = www
            .as_deref()
            .and_then(crate::auth::challenge::parse_www_authenticate)
            .is_some_and(|challenge| challenge.error.as_deref() == Some("insufficient_scope"));
        if response.status == 401 || insufficient {
            let (retried, _) = send(true).await?;
            if retried.status == 401 {
                let mut failure = McpError::new(
                    McpErrorKind::Auth,
                    "Server returned 401 after successful authentication",
                );
                failure.phase = Some("auth".into());
                return Err(ProbeFetchError::NeedsAuth(Box::new(failure)));
            }
            return Ok(retried);
        }
    }
    Ok(response)
}
