//! Port of senpi mcp/auth/registered-redirect.ts (senpi#6327): reuse only a dynamic registration
//! that names the authorize flow's own redirect URI, staging new registrations until code exchange succeeds.
use super::token_store::McpStoredAuth;
use serde_json::{Value, json};

/// The provider options that decide which registration a flow uses.
#[derive(Clone, Copy, Debug, Default)]
pub struct RegistrationFlow<'a> {
    pub redirect_url: Option<&'a str>,
    /// The flow sends an authorize request to `redirect_url`.
    pub require_registered_redirect: bool,
}

// Matched against the redirect URIs the client sent when it registered: an authorization server may echo them
// rewritten. A registration saved before that record existed falls back to the echo; one with neither is kept.
fn registration_names_redirect(registration: &Value, redirect_url: Option<&str>) -> bool {
    let Some(redirect_url) = redirect_url else {
        return true;
    };
    let Some(registered) = registration
        .get("redirectUrisSent")
        .or_else(|| registration.get("redirect_uris"))
        .and_then(Value::as_array)
    else {
        return true;
    };
    registered
        .iter()
        .any(|uri| uri.as_str() == Some(redirect_url))
}

/// The registration a flow uses: for an authorizing flow, the signed-in or staged client naming its redirect URI.
pub fn registration_for_flow(
    record: Option<&McpStoredAuth>,
    flow: &RegistrationFlow<'_>,
) -> Option<Value> {
    let record = record?;
    if !flow.require_registered_redirect {
        return record.client_info.clone();
    }
    [
        record.client_info.as_ref(),
        record.pending_client_info.as_ref(),
    ]
    .into_iter()
    .flatten()
    .find(|registration| registration_names_redirect(registration, flow.redirect_url))
    .cloned()
}

fn uses_staged_registration(record: Option<&McpStoredAuth>, flow: &RegistrationFlow<'_>) -> bool {
    let Some(record) = record else {
        return false;
    };
    if !flow.require_registered_redirect {
        return false;
    }
    if record
        .client_info
        .as_ref()
        .is_some_and(|reg| registration_names_redirect(reg, flow.redirect_url))
    {
        return false;
    }
    record
        .pending_client_info
        .as_ref()
        .is_some_and(|reg| registration_names_redirect(reg, flow.redirect_url))
}

/// The stored grant as a flow may present it: a staged client has no grant, so the refresh token is withheld.
pub fn grant_for_flow(
    record: Option<McpStoredAuth>,
    flow: &RegistrationFlow<'_>,
) -> Option<McpStoredAuth> {
    let mut record = record?;
    if uses_staged_registration(Some(&record), flow) {
        record.refresh_token = None;
    }
    Some(record)
}

/// Stores a registration. An authorizing flow stages a client that would replace the signed-in one; otherwise a
/// replaced client's refresh token goes with it, because the SDK would present it under the new `client_id`.
pub fn with_registration(
    current: Option<McpStoredAuth>,
    mut client_info: Value,
    flow: &RegistrationFlow<'_>,
) -> McpStoredAuth {
    fn client_id_of(value: Option<&Value>) -> Option<&str> {
        value
            .and_then(|reg| reg.get("client_id"))
            .and_then(Value::as_str)
    }
    let new_id = client_id_of(Some(&client_info)).map(str::to_owned);
    let known = current.as_ref().is_some_and(|current| {
        [
            current.client_info.as_ref(),
            current.pending_client_info.as_ref(),
        ]
        .into_iter()
        .any(|registration| new_id.is_some() && client_id_of(registration) == new_id.as_deref())
    });
    // The SDK registers with `clientMetadata.redirect_uris`, which is exactly the flow's redirect URL.
    if !known
        && let Some(redirect_url) = flow.redirect_url
        && let Some(object) = client_info.as_object_mut()
    {
        object.insert("redirectUrisSent".into(), json!([redirect_url]));
    }
    let replaced = current
        .as_ref()
        .and_then(|current| client_id_of(current.client_info.as_ref()).map(str::to_owned));
    let mut next = current.unwrap_or_default();
    if replaced.is_none() || replaced == new_id {
        next.client_info = Some(client_info);
        return next;
    }
    if flow.require_registered_redirect {
        next.pending_client_info = Some(client_info);
        return next;
    }
    next.client_info = Some(client_info);
    next.refresh_token = None;
    next
}

/// After an authorizing flow's code exchange: its staged client replaces the signed-in one and that client's grant.
pub fn with_promoted_registration(
    current: Option<McpStoredAuth>,
    flow: &RegistrationFlow<'_>,
) -> Option<McpStoredAuth> {
    let mut current = current?;
    if current.pending_client_info.is_none() || !uses_staged_registration(Some(&current), flow) {
        return Some(current);
    }
    current.client_info = current.pending_client_info.take();
    current.refresh_token = None;
    Some(current)
}
