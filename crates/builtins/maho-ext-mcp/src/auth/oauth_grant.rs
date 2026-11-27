//! Port of senpi mcp/auth/oauth-grant.ts (senpi#2861 / #6366): OAuth grant provenance,
//! active grant binding, durable credential commitments, proof validation and epoch selection.
use super::{
    oauth_issuer::canonical_authorization_server_issuer,
    oauth_provider::{OAuthTokens, token_expires_at},
    token_store::{McpAuthorization, McpAuthorizationProof, McpStoredAuth},
};
use crate::config_identity_key::{is_ephemeral_identity, keyed_identity};
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::Path};

#[derive(Clone, Debug)]
pub struct McpGrantContext<'a> {
    pub server_name: &'a str,
    pub server_url: &'a str,
    pub agent_dir: &'a Path,
    pub client_id: Option<&'a str>,
    pub client_metadata_url: Option<&'a str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantBinding {
    pub issuer: Option<String>,
    pub resource: Option<String>,
    pub client_identity: Option<String>,
    pub scope: Option<String>,
    pub refresh_scope: Option<String>,
}

fn is_uuid_v4(value: &str) -> bool {
    static UUID_V4: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$")
            .expect("uuid v4 regex")
    });
    UUID_V4.is_match(value)
}

fn is_hex64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

pub fn random_uuid_v4() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        static FALLBACK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let seq = FALLBACK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        bytes[..8].copy_from_slice(&seq.to_le_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15],
    )
}

fn authorization_epoch(record: &McpStoredAuth) -> Option<&str> {
    let auth = record.authorization.as_ref()?;
    (auth.version == 1 && is_uuid_v4(&auth.id)).then_some(auth.id.as_str())
}

pub fn normalize_granted_scope(scope: Option<&str>) -> Option<String> {
    static SCOPE_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^[\x21\x23-\x5b\x5d-\x7e]+(?: +[\x21\x23-\x5b\x5d-\x7e]+)*$")
            .expect("scope regex")
    });
    let scope = scope?;
    if !SCOPE_RE.is_match(scope) {
        return None;
    }
    let unique: BTreeSet<&str> = scope.split(' ').filter(|token| !token.is_empty()).collect();
    if unique.is_empty() {
        return None;
    }
    Some(unique.into_iter().collect::<Vec<_>>().join(" "))
}

pub fn stored_grant_issuer(record: Option<&McpStoredAuth>) -> Option<String> {
    let record = record?;
    record.issuer.clone().or_else(|| {
        record
            .extra
            .get("discoveryState")
            .and_then(|state| state.get("authorizationServerUrl"))
            .and_then(Value::as_str)
            .filter(|url| !url.is_empty())
            .map(str::to_owned)
    })
}

pub fn freeze_grant_attribution(record: Option<McpStoredAuth>) -> McpStoredAuth {
    let mut next = record.unwrap_or_default();
    if next.issuer.is_some() || (next.access_token.is_none() && next.refresh_token.is_none()) {
        return next;
    }
    if let Some(issuer) = stored_grant_issuer(Some(&next)) {
        next.issuer = Some(issuer);
    } else {
        next.refresh_token = None;
    }
    next
}

pub fn stored_auth_to_tokens(record: Option<&McpStoredAuth>, now: f64) -> Option<OAuthTokens> {
    let record = record?;
    let access = record.access_token.as_ref().filter(|s| !s.is_empty())?;
    let expires_in = record
        .expires_at
        .map(|t| ((t - now) / 1000.0).ceil().max(0.0));
    let issuer = stored_grant_issuer(Some(record));
    Some(OAuthTokens {
        access_token: access.clone(),
        refresh_token: issuer.as_ref().and(record.refresh_token.clone()),
        token_type: "Bearer".into(),
        scope: normalize_granted_scope(record.scope.as_deref()),
        expires_in,
        issuer,
    })
}

/// Maps one response; the caller decides which preceding fields it is entitled to inherit.
pub fn merge_tokens_into_stored_auth(
    current: McpStoredAuth,
    tokens: &OAuthTokens,
    now: f64,
) -> McpStoredAuth {
    let mut next = current;
    next.access_token = Some(tokens.access_token.clone());
    if let Some(refresh) = &tokens.refresh_token {
        next.refresh_token = Some(refresh.clone());
    }
    next.scope = normalize_granted_scope(tokens.scope.as_deref());
    next.expires_at = token_expires_at(tokens, now);
    if let Some(issuer) = &tokens.issuer {
        next.issuer = Some(issuer.clone());
    }
    next
}

pub struct McpTokenResponse<'a> {
    pub tokens: &'a OAuthTokens,
    pub resource: &'a str,
    pub fresh: bool,
    pub presented: Option<&'a McpStoredAuth>,
    pub client_info: Option<Value>,
    pub now: f64,
}

/// An SDK response has no locked predecessor; only explicit fresh acquisition can establish its initial proof.
pub fn merge_oauth_token_response(
    current: McpStoredAuth,
    context: &McpGrantContext<'_>,
    response: McpTokenResponse<'_>,
) -> McpStoredAuth {
    let McpTokenResponse {
        tokens,
        resource,
        fresh,
        presented,
        client_info,
        now,
    } = response;
    let pending_id = current
        .pending_client_info
        .as_ref()
        .and_then(|info| info.get("client_id"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let selected_id = client_info
        .as_ref()
        .and_then(|info| info.get("client_id"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    // Attribute a response to the client selected before the request, never to a later store writer.
    let mut active = current.clone();
    active.client_info = client_info;
    if pending_id == selected_id {
        active.pending_client_info = None;
    }
    let mut probe = active.clone();
    probe.issuer = tokens.issuer.clone();
    probe.resource = Some(resource.to_owned());
    let binding = grant_binding(&probe, context);
    let can_inherit = !fresh
        && presented.is_some_and(|presented| {
            let presented_binding = grant_binding(presented, context);
            presented_binding == grant_binding(&current, context)
                && presented_binding == binding
                && presented.access_token == current.access_token
                && presented.refresh_token == current.refresh_token
                && presented.authorization.as_ref().map(|a| &a.id)
                    == current.authorization.as_ref().map(|a| &a.id)
        });
    let mut base = active;
    base.issuer = tokens.issuer.clone();
    base.resource = Some(resource.to_owned());
    base.refresh_token = if can_inherit {
        presented.and_then(|p| p.refresh_token.clone())
    } else {
        None
    };
    base.scope = None;
    base.refresh_scope = None;
    base.authorization = None;
    let mut next = merge_tokens_into_stored_auth(base, tokens, now);
    if fresh {
        next.refresh_scope = next.scope.clone();
        with_grant_authorization(next, context, None)
    } else {
        next.authorization = Some(McpAuthorization {
            version: 1,
            id: random_uuid_v4(),
            proof: None,
        });
        next
    }
}

/// Active binding excludes pending registrations and display/redirect metadata.
pub fn grant_binding(record: &McpStoredAuth, context: &McpGrantContext<'_>) -> GrantBinding {
    let issuer = stored_grant_issuer(Some(record));
    let registration = record.client_info.as_ref();
    let reg_client_id = registration
        .and_then(|reg| reg.get("client_id"))
        .and_then(Value::as_str);
    let client_id = context
        .client_id
        .or(context.client_metadata_url)
        .or(reg_client_id);
    let configured = context.client_id.is_some() || context.client_metadata_url.is_some();
    let reg_issuer = registration
        .and_then(|reg| reg.get("issuer"))
        .and_then(Value::as_str);
    let client_issuer = if configured {
        issuer.as_deref()
    } else {
        reg_issuer.or(issuer.as_deref())
    };
    let client_identity = client_id.map(|client_id| {
        let client_secret = if configured {
            None
        } else {
            registration
                .and_then(|reg| reg.get("client_secret"))
                .and_then(Value::as_str)
        };
        let auth_method = if configured {
            None
        } else {
            registration
                .and_then(|reg| reg.get("token_endpoint_auth_method"))
                .and_then(Value::as_str)
        };
        let canonical_issuer = client_issuer.map(canonical_authorization_server_issuer);
        let payload = json!([
            "mcp-oauth-client-v1",
            client_id,
            client_secret,
            auth_method,
            canonical_issuer,
        ])
        .to_string();
        keyed_identity(Some(context.agent_dir), &payload)
    });
    GrantBinding {
        issuer: issuer.map(|issuer| canonical_authorization_server_issuer(&issuer)),
        resource: record.resource.clone(),
        client_identity,
        scope: normalize_granted_scope(record.scope.as_deref()),
        refresh_scope: normalize_granted_scope(record.refresh_scope.as_deref()),
    }
}

fn credential_commitment(
    record: &McpStoredAuth,
    context: &McpGrantContext<'_>,
    id: Option<&str>,
) -> String {
    let binding = grant_binding(record, context);
    let payload = json!([
        "mcp-oauth-credential-v1",
        context.server_name,
        context.server_url,
        id,
        record.access_token,
        record.refresh_token,
        binding.issuer,
        binding.resource,
        binding.client_identity,
        binding.scope,
        binding.refresh_scope,
    ])
    .to_string();
    keyed_identity(Some(context.agent_dir), &payload)
}

pub fn valid_grant_proof<'a>(
    record: &'a McpStoredAuth,
    context: &McpGrantContext<'_>,
) -> Option<&'a McpAuthorizationProof> {
    let auth = record.authorization.as_ref()?;
    if auth.version != 1 || !is_uuid_v4(&auth.id) {
        return None;
    }
    let proof = auth.proof.as_ref()?;
    if !is_hex64(&proof.credential_identity)
        || proof
            .previous_credential_identity
            .as_deref()
            .is_some_and(|prev| !is_hex64(prev))
        || proof.issuer.is_empty()
        || proof.resource.is_empty()
        || !is_hex64(&proof.client_identity)
        || proof.scope.is_empty()
        || proof.refresh_scope.as_deref().is_some_and(str::is_empty)
    {
        return None;
    }
    let binding = grant_binding(record, context);
    if binding.issuer.as_deref() != Some(proof.issuer.as_str())
        || binding.resource.as_deref() != Some(proof.resource.as_str())
        || binding.client_identity.as_deref() != Some(proof.client_identity.as_str())
        || binding.scope.as_deref() != Some(proof.scope.as_str())
        || binding.refresh_scope != proof.refresh_scope
    {
        return None;
    }
    (proof.credential_identity == credential_commitment(record, context, Some(&auth.id)))
        .then_some(proof)
}

/// Only the refresh transaction passes a predecessor, read while holding its existing token-store lock.
pub fn with_grant_authorization(
    mut record: McpStoredAuth,
    context: &McpGrantContext<'_>,
    previous: Option<&McpStoredAuth>,
) -> McpStoredAuth {
    let predecessor = previous.and_then(|prev| valid_grant_proof(prev, context));
    let binding = grant_binding(&record, context);
    let same_binding = predecessor.is_some_and(|prev| {
        binding.issuer.as_deref() == Some(prev.issuer.as_str())
            && binding.resource.as_deref() == Some(prev.resource.as_str())
            && binding.client_identity.as_deref() == Some(prev.client_identity.as_str())
            && binding.scope.as_deref() == Some(prev.scope.as_str())
            && binding.refresh_scope == prev.refresh_scope
    });
    let id = if same_binding {
        previous
            .and_then(|prev| prev.authorization.as_ref())
            .map(|auth| auth.id.clone())
            .unwrap_or_else(random_uuid_v4)
    } else {
        random_uuid_v4()
    };
    let credential_identity = credential_commitment(&record, context, Some(&id));
    let proof = if let (Some(issuer), Some(resource), Some(client_identity), Some(scope)) = (
        binding.issuer,
        binding.resource,
        binding.client_identity,
        binding.scope,
    ) && !is_ephemeral_identity(Some(&credential_identity))
    {
        Some(McpAuthorizationProof {
            credential_identity,
            previous_credential_identity: previous
                .map(|prev| credential_commitment(prev, context, authorization_epoch(prev))),
            issuer,
            resource,
            client_identity,
            scope,
            refresh_scope: binding.refresh_scope,
        })
    } else {
        None
    };
    record.authorization = Some(McpAuthorization {
        version: 1,
        id,
        proof,
    });
    record
}

pub fn oauth_grant_identity(record: &McpStoredAuth, context: &McpGrantContext<'_>) -> String {
    let epoch = authorization_epoch(record);
    match valid_grant_proof(record, context) {
        None => {
            let commitment = credential_commitment(record, context, epoch);
            let payload = json!(["mcp-oauth-opaque-v1", commitment]).to_string();
            keyed_identity(Some(context.agent_dir), &payload)
        }
        Some(proof) => {
            let payload = json!([
                "mcp-oauth-authorization-v1",
                context.server_name,
                context.server_url,
                epoch,
                proof.issuer,
                proof.resource,
                proof.client_identity,
                proof.scope,
                proof.refresh_scope,
            ])
            .to_string();
            keyed_identity(Some(context.agent_dir), &payload)
        }
    }
}
