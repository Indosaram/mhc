//! Port of omo-senpi `components/model-profile/request-auth.ts` at omo `455dee62`: session-start
//! emulation of the first turn's request-auth resolution for one profile candidate.
//!
//! senpi resolves auth lazily, on the first request, and `modelRegistry.getAvailable()` lists every
//! provider with STORED credentials - including an OAuth login whose refresh token the provider now
//! rejects. The first turn goes through the model runtime, which (a) rotates over a provider's
//! credential SLOTS when it holds more than one account and rotation is allowed (a pinned account
//! wins; [`crate::credential_policy`] reads when rotation is off and only the flat credential
//! counts), and (b) resolves the model's own configured headers on top of the provider credential.
//! Both are reproduced here through the registry's slot-aware auth overloads, so the walk skips what
//! that turn could not use:
//!
//!   - provider scope: each candidate slot is passed to
//!     `ModelRegistry::get_provider_auth_for_slot(provider, slot)` (senpi
//!     `runtime.getAuth(provider, {slotName})`), and the slot that RESOLVED is threaded into the
//!     model-scope probe `get_api_key_and_headers_for_slot(model, slot)` (senpi
//!     `runtime.getAuth(model, {slotName})`) - never the flat credential in its place. No eligible
//!     slot resolving is `refresh` (the stored login could not be refreshed) or `credentials` (any
//!     other resolution failure, e.g. a failing `!command` key);
//!   - model scope (`request`): the provider credential resolved but this model's request
//!     configuration did not, so only that candidate is skipped and its siblings stay eligible;
//!   - the rotation policy reads the registry's own `get_provider_auth_status` port
//!     ([`crate::credential_policy`]), so a runtime API key turns rotation off exactly as upstream.
//!
//! The exact shared signatures are recorded in `.omo/authoring/model-profile.md` section 2. Account
//! presence is never used as a substitute for the probe.
//!
//! The class/code fingerprint is senpi's `${error.name}/${error.code}`, read from the typed
//! `ExtensionFailure::class`/`::code` - never from the message text, which carries URLs, response
//! bodies, tokens and shell commands. Only that fingerprint leaves this module by default;
//! [`sanitized_auth_error_detail`] is for opt-in diagnostics.

use fancy_regex::Regex;
use maho_ai::model::Model;
use maho_ext_api::{
    CredentialAccountSummary, ExtensionFailure, ModelRegistry, ResolvedRequestAuth,
};
use std::sync::LazyLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthFailureReason {
    Refresh,
    Credentials,
    Request,
}

impl AuthFailureReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Refresh => "refresh",
            Self::Credentials => "credentials",
            Self::Request => "request",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthFailure {
    pub provider: String,
    pub model: String,
    pub reason: AuthFailureReason,
    /// Error class and code fingerprint (for example `ModelsError/oauth`), never the message.
    pub error_kind: String,
    /// The failure, kept for opt-in diagnostics; never serialized into notices or details.
    pub error: Option<ExtensionFailure>,
}

/// The subset of senpi's credential reading the probe performs: the named slots and the pinned one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CredentialSlots {
    pub names: Vec<String>,
    pub pinned: Option<String>,
}

/// Mirrors senpi's pool reading: a credential with an `accounts` array is a pool of named slots and
/// the pinned account names the only slot rotation may use; anything else is the flat single
/// credential. The native `get_credential_accounts` reports the same data (names plus a per-account
/// `pinned` flag).
pub async fn credential_slots(registry: &dyn ModelRegistry, provider: &str) -> CredentialSlots {
    let Ok(accounts) = registry.get_credential_accounts(provider).await else {
        return CredentialSlots::default();
    };
    let pinned = accounts
        .iter()
        .find(|account: &&CredentialAccountSummary| account.pinned)
        .map(|account| account.name.clone());
    CredentialSlots {
        names: accounts.into_iter().map(|account| account.name).collect(),
        pinned,
    }
}

/// Upstream `errorKind(error)` is `${error.name}/${error.code}`, or just the class name when the
/// error carries no machine code. Read from the typed classification - the message is NEVER
/// inspected, so a message whose leading token is key material cannot leak it into `errorKind`.
fn error_kind(error: &ExtensionFailure) -> String {
    let class = error.class.as_deref().filter(|class| !class.is_empty()).unwrap_or("Error");
    match error.code.as_deref().filter(|code| !code.is_empty()) {
        Some(code) => format!("{class}/{code}"),
        None => class.to_owned(),
    }
}

/// senpi `providerFailureReason(error)`: the machine code `oauth` is the refresh signal (a rejected
/// token, a network error and an exchange timeout all map onto it), every other resolution failure is
/// `credentials`. The message is not inspected, so a transient failure is reported with the same
/// neutral guidance as a rejected one.
fn provider_failure_reason(error: &ExtensionFailure) -> AuthFailureReason {
    if error.code.as_deref() == Some("oauth") {
        AuthFailureReason::Refresh
    } else {
        AuthFailureReason::Credentials
    }
}

/// Returns the failure that would stop the first turn on this candidate, or `None` when the
/// candidate is usable.
pub async fn probe_request_auth(
    registry: &dyn ModelRegistry,
    may_rotate: bool,
    provider: &str,
    model_id: &str,
    model: &Model,
) -> Option<AuthFailure> {
    let slots = credential_slots(registry, provider).await;
    // The engine's selection: without rotation only the flat credential is used; with it, a pin is
    // the only slot, otherwise any resolving slot serves.
    let candidates: Vec<Option<String>> = if !may_rotate {
        vec![None]
    } else if let Some(pinned) = slots.pinned.clone() {
        vec![Some(pinned)]
    } else if slots.names.is_empty() {
        vec![None]
    } else {
        slots.names.iter().cloned().map(Some).collect()
    };

    let mut resolved_slot: Option<String> = None;
    let mut resolved = false;
    let mut last_error: Option<ExtensionFailure> = None;
    for slot in &candidates {
        match registry.get_provider_auth_for_slot(provider, slot.as_deref()).await {
            // `Ok(None)` means the provider needs no credential (headers-only compatibility config),
            // which the first turn also accepts; only an error is a failure.
            Ok(_) => {
                resolved = true;
                resolved_slot = slot.clone();
                break;
            }
            Err(error) => last_error = Some(error),
        }
    }
    if !resolved {
        let error = last_error.unwrap_or_else(|| ExtensionFailure::new("credential resolution failed"));
        return Some(AuthFailure {
            provider: provider.to_owned(),
            model: model_id.to_owned(),
            reason: provider_failure_reason(&error),
            error_kind: error_kind(&error),
            error: Some(error),
        });
    }

    // The MODEL overload carries the SAME slot the provider scope resolved on, so the model's own
    // headers resolve against that account rather than the flat/default credential.
    match registry.get_api_key_and_headers_for_slot(model, resolved_slot.as_deref()).await {
        Ok(ResolvedRequestAuth { .. }) => None,
        Err(error) => Some(AuthFailure {
            provider: provider.to_owned(),
            model: model_id.to_owned(),
            reason: AuthFailureReason::Request,
            error_kind: error_kind(&error),
            error: Some(error),
        }),
    }
}

const DETAIL_MAX_LENGTH: usize = 240;

// Any field whose NAME says it carries a secret, in `name: value`, `name=value` or JSON `"name":
// "value"` form; the whole value is dropped, whatever its length. `fancy-regex` is used because the
// pattern's quote backreference (`\1`) is not expressible in the linear-time `regex` crate.
static SECRET_FIELD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)(["']?)\b([\w-]*(?:token|secret|password|passwd|authorization|api[_-]?key|apikey|access[_-]?key|refresh|credential|cookie|session)[\w-]*)\1(\s*[:=]\s*)("[^"]*"|'[^']*'|[^\s,;}&]+(?:\s+[^\s,;}&]+)?)"#,
    )
    .expect("the secret-field pattern is a valid expression")
});
static SHELL_COMMAND: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)shell command:.*$").expect("valid expression"));
static BODY_OR_STACK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(body|stack|details)=.*$").expect("valid expression"));
static BEARER_SCHEME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)\b(bearer|basic)\s+[^\s,;"']+"#).expect("valid expression"));
static URL_USERINFO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(\b[a-z][a-z0-9+.-]*://)[^/\s@]*@").expect("valid expression")
});
static URL_QUERY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(\b[a-z][a-z0-9+.-]*://[^\s?#]*)[?#][^\s]*").expect("valid expression")
});
static OPAQUE_STRING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Za-z0-9_+/=-]{24,}").expect("valid expression"));

fn replace_all(pattern: &Regex, text: &str, replacement: &str) -> String {
    pattern.replace_all(text, replacement).into_owned()
}

// One redacted line for the failure. Shell commands and response bodies are dropped whole (a
// `!command` key or header can embed a secret), secret-named fields lose their value (including a
// two-word `Authorization: Bearer <token>`), bearer/basic schemes lose their credential, URL userinfo
// and query strings are dropped, remaining long opaque strings are masked, and stacks never appear.
fn sanitized_line(error: &ExtensionFailure) -> String {
    // The class name is kept verbatim (it can itself read like a secret-named field, for example
    // `OAuthRefreshExchangeError:`); only the message is redacted.
    let class = error.class.as_deref().filter(|class| !class.is_empty());
    let first_line = error.message.split('\n').next().unwrap_or_default();
    let redacted = replace_all(&SHELL_COMMAND, first_line, "shell command: <redacted>");
    let redacted = replace_all(&BODY_OR_STACK, &redacted, "$1=<redacted>");
    let redacted = replace_all(&SECRET_FIELD, &redacted, "$1$2$1$3<redacted>");
    let redacted = replace_all(&BEARER_SCHEME, &redacted, "$1 <redacted>");
    let redacted = replace_all(&URL_USERINFO, &redacted, "$1<redacted>@");
    let redacted = replace_all(&URL_QUERY, &redacted, "$1?<redacted>");
    let redacted = replace_all(&OPAQUE_STRING, &redacted, "<redacted>");
    let line = match class {
        Some(class) => format!("{class}: {redacted}"),
        None => redacted,
    };
    line.chars().take(DETAIL_MAX_LENGTH).collect()
}

/// Redacted, bounded description of an auth failure for opt-in (debug) diagnostics only.
///
/// Outstanding signature: senpi walks the `error.cause` chain (up to 4 links, joined with ` <- `) so
/// a wrapped `OAuthRefreshExchangeError` keeps its class name in the detail. `ExtensionFailure` has
/// no `cause` field yet, so this reports the top failure only; recorded in
/// `.omo/authoring/model-profile.md` section 2.
pub fn sanitized_auth_error_detail(error: Option<&ExtensionFailure>) -> String {
    error.map(sanitized_line).unwrap_or_default()
}
