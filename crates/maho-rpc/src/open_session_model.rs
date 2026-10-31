//! An `open_session` that names a model must run on it or fail (senpi `rpc/open-session-model.ts`,
//! senpi#2906). The runtime factory reports an unresolvable model only as a diagnostic and falls back to
//! the default model, which the standalone CLI treats as fatal but the host used to ignore.
use crate::session_registry::{RpcSessionLaunchProfile, RpcSessionRegistryError};

/// The diagnostic code the session-options builder tags an unresolvable requested model with.
pub const MODEL_UNRESOLVED_CODE: &str = "model_unresolved";

/// One runtime diagnostic as this check reads it: its optional `code` and its message.
pub struct OpenDiagnostic<'a> {
    pub code: Option<&'a str>,
    pub message: &'a str,
}

pub fn assert_requested_model_resolved<'a>(
    profile: &RpcSessionLaunchProfile,
    diagnostics: impl IntoIterator<Item = OpenDiagnostic<'a>>,
) -> Result<(), RpcSessionRegistryError> {
    let Some((provider, model_id)) = profile.runtime.creation_model.as_ref() else {
        return Ok(());
    };
    let Some(unresolved) = diagnostics
        .into_iter()
        .find(|diagnostic| diagnostic.code == Some(MODEL_UNRESOLVED_CODE))
    else {
        return Ok(());
    };
    Err(RpcSessionRegistryError::new(
        "open_failed",
        Some(format!("model_unavailable: {}", unresolved.message).as_str()),
        Some(serde_json::json!({
            "reason": "model_unavailable",
            "requestedModel": format!("{provider}/{model_id}"),
        })),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(model: Option<(&str, &str)>) -> RpcSessionLaunchProfile {
        let mut profile = RpcSessionLaunchProfile::default();
        profile.runtime.creation_model = model.map(|(p, m)| (p.to_owned(), m.to_owned()));
        profile
    }

    #[test]
    fn a_named_unresolved_model_fails_the_open() {
        let error = assert_requested_model_resolved(
            &profile(Some(("anthropic", "nope"))),
            [OpenDiagnostic {
                code: Some("model_unresolved"),
                message: "Model not found",
            }],
        )
        .expect_err("refused");
        assert_eq!(
            error.to_string(),
            "open_failed: model_unavailable: Model not found"
        );
        assert_eq!(
            error.detail.expect("detail")["requestedModel"],
            "anthropic/nope"
        );
    }

    #[test]
    fn an_unnamed_model_or_other_diagnostics_pass() {
        let unresolved = || {
            [OpenDiagnostic {
                code: Some("model_unresolved"),
                message: "x",
            }]
        };
        assert!(assert_requested_model_resolved(&profile(None), unresolved()).is_ok());
        assert!(
            assert_requested_model_resolved(
                &profile(Some(("a", "b"))),
                [OpenDiagnostic {
                    code: None,
                    message: "warning"
                }]
            )
            .is_ok()
        );
    }
}
