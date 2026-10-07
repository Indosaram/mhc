//! Port of omo-senpi `components/model-profile/notice.ts` at omo `455dee62`: the notice content and
//! the auth-failure details emitted with it.
//!
//! The notice text is model/user-facing prose carried verbatim from the pinned source; it is not
//! pinned by tests (see `parity.d/1008.md`).

use maho_ext_api::JsonValue;

use crate::request_auth::AuthFailure;
use crate::resolve::{ModelProfileResolution, ModelProfileSummary};

const MID_SESSION_NOTE: &str = "mid-session fallback follows senpi's retry chains";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthFailedDetail {
    pub provider: String,
    pub model: String,
    pub reason: &'static str,
}

pub fn auth_failed_details(failures: &[AuthFailure]) -> Option<JsonValue> {
    if failures.is_empty() {
        return None;
    }
    let entries: Vec<JsonValue> = failures
        .iter()
        .map(|failure| {
            JsonValue::Object(
                [
                    ("provider".to_owned(), JsonValue::String(failure.provider.clone())),
                    ("model".to_owned(), JsonValue::String(failure.model.clone())),
                    ("reason".to_owned(), JsonValue::String(failure.reason.as_str().to_owned())),
                ]
                .into_iter()
                .collect(),
            )
        })
        .collect();
    Some(JsonValue::Object(
        [("authFailed".to_owned(), JsonValue::Array(entries))].into_iter().collect(),
    ))
}

fn profile_label(profile: &ModelProfileSummary) -> String {
    if profile.display_name != profile.id {
        format!("\"{}\" ({})", profile.id, profile.display_name)
    } else {
        format!("\"{}\"", profile.id)
    }
}

// The `/login` slash command exists only in the interactive terminal, which this component never
// applies to: a desktop (rpc) client re-authenticates from its provider settings, and a headless
// run needs an interactive session for the login flow.
fn reauthenticate(provider: &str, mode: Option<&str>) -> String {
    if mode == Some("rpc") {
        format!("re-authenticate {provider} in Provider authentication settings")
    } else {
        format!("re-authenticate {provider} in an interactive session with /login {provider}")
    }
}

fn skipped_note(failure: &AuthFailure, mode: Option<&str>) -> String {
    match failure.reason {
        crate::request_auth::AuthFailureReason::Refresh => format!(
            "skipped {}: its login could not be refreshed at session start (if it has expired, {}; a connectivity problem clears on the next start)",
            failure.provider,
            reauthenticate(&failure.provider, mode)
        ),
        crate::request_auth::AuthFailureReason::Credentials => format!(
            "skipped {}: its credentials did not resolve at session start (check that provider's credential configuration)",
            failure.provider
        ),
        crate::request_auth::AuthFailureReason::Request => format!(
            "skipped {}/{}: its request configuration did not resolve (check that model's headers in models.json)",
            failure.provider, failure.model
        ),
    }
}

pub fn auth_failure_note(failures: &[AuthFailure], mode: Option<&str>) -> String {
    failures
        .iter()
        .map(|failure| skipped_note(failure, mode))
        .collect::<Vec<_>>()
        .join("; ")
}

pub fn notice_content(
    resolution: &ModelProfileResolution,
    auth_failures: &[AuthFailure],
    mode: Option<&str>,
) -> String {
    match resolution {
        ModelProfileResolution::Resolved { profile, provider, model_id, reasoning, skipped } => {
            let model = format!("{provider}/{model_id}");
            let reasoning = reasoning
                .as_ref()
                .map(|reasoning| format!(" {reasoning}"))
                .unwrap_or_default();
            let skipped = if skipped.is_empty() {
                String::new()
            } else {
                format!(" (skipped: {})", skipped.join(", "))
            };
            let auth = if auth_failures.is_empty() {
                String::new()
            } else {
                format!("; {}", auth_failure_note(auth_failures, mode))
            };
            format!(
                "OmO Native: model profile {} selected {model}{reasoning}{skipped}{auth}; {MID_SESSION_NOTE}",
                profile_label(profile)
            )
        }
        ModelProfileResolution::Unavailable { profile, chain } => {
            if auth_failures.is_empty() {
                format!(
                    "OmO Native: model profile {} has no available model; none of the chain is in this session's model registry ({}); keeping senpi's default model",
                    profile_label(profile),
                    chain.join(", ")
                )
            } else {
                format!(
                    "OmO Native: model profile {} has no model whose credentials resolve; {}; keeping senpi's default model",
                    profile_label(profile),
                    auth_failure_note(auth_failures, mode)
                )
            }
        }
        ModelProfileResolution::Empty { profile } => format!(
            "OmO Native: model profile {} defines no models; keeping senpi's default model",
            profile_label(profile)
        ),
        ModelProfileResolution::Unknown { message, .. } => format!("OmO Native: {message}"),
    }
}
