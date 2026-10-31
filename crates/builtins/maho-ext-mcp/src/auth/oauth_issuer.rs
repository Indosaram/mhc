//! Port of senpi mcp/auth/oauth-issuer.ts (senpi#4194): authorization server issuer trust.
use super::{
    oauth::OAuthServerInfo,
    oauth_errors::{OAuthFailureKind, OAuthFlowError},
    token_store::McpStoredAuth,
};
use serde_json::Value;

/// Credential stamps tolerate only the slash spelling of an origin-only URL, not URL normalization.
pub fn canonical_authorization_server_issuer(issuer: &str) -> String {
    static ORIGIN_SLASH: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^(https?://[^/?#]+)/$").expect("issuer pattern")
    });
    ORIGIN_SLASH.replace(issuer, "$1").into_owned()
}

/// RFC 8414 section 3.3: identifiers from metadata are compared verbatim, not as normalized URLs.
pub fn validated_authorization_server_issuer(
    info: &OAuthServerInfo,
    server_name: &str,
) -> Result<String, Box<OAuthFlowError>> {
    // The root-metadata fallback spells the origin with a trailing slash. Only that configured-resource fallback
    // is normalized; identifiers provided by protected resource metadata remain exact.
    let origin = url::Url::parse(&info.authorization_server_url)
        .ok()
        .map(|url| url.origin().ascii_serialization());
    let advertised = info
        .resource_metadata
        .get("authorization_servers")
        .and_then(Value::as_array)
        .is_some_and(|servers| !servers.is_empty());
    let issuer = if !advertised
        && origin.is_some_and(|origin| info.authorization_server_url == format!("{origin}/"))
    {
        canonical_authorization_server_issuer(&info.authorization_server_url)
    } else {
        info.authorization_server_url.clone()
    };
    let metadata = &info.authorization_server_metadata;
    if !metadata.is_null()
        && metadata.get("issuer").and_then(Value::as_str) != Some(issuer.as_str())
    {
        return Err(Box::new(OAuthFlowError::new(
            OAuthFailureKind::NeedsAuth,
            format!(
                "MCP server {server_name} authorization server metadata issuer mismatch; restart authorization after correcting the server metadata."
            ),
        )));
    }
    Ok(issuer)
}

/// Every stamp a record carries (grant and registration) must name `issuer`; an absent stamp does not conflict.
pub fn credentials_match_issuer(record: Option<&McpStoredAuth>, issuer: &str) -> bool {
    let grant_issuer = record.and_then(|record| record.issuer.clone());
    let client_issuer = record
        .and_then(|record| record.client_info.as_ref())
        .and_then(|info| info.get("issuer"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| grant_issuer.clone());
    let wanted = canonical_authorization_server_issuer(issuer);
    [grant_issuer, client_issuer].iter().all(|stamp| {
        stamp
            .as_deref()
            .is_none_or(|stamp| canonical_authorization_server_issuer(stamp) == wanted)
    })
}
