pub use crate::types::{
    PROVIDER_DIAGNOSTIC_MAX_BYTES, ProviderDiagnostic, ProviderDiagnosticCategory,
    ProviderDiagnosticEvidence, ProviderDiagnosticRoute,
};

pub fn sanitize_provider_diagnostic(value: &serde_json::Value) -> Option<ProviderDiagnostic> {
    let diagnostic: ProviderDiagnostic = serde_json::from_value(value.clone()).ok()?;
    crate::types::sanitize_provider_diagnostic(&diagnostic)
}

pub fn read_provider_diagnostic(
    error: &(dyn std::error::Error + 'static),
) -> Option<ProviderDiagnostic> {
    let diagnostic = super::provider_diagnostic_carrier::peek_provider_diagnostic(error)?;
    crate::types::sanitize_provider_diagnostic(diagnostic)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn contradictory_evidence_is_dropped_when_deserialized() {
        let value = json!({"category":"quota", "httpStatus":401,
            "code":"insufficient_quota", "evidence":"structured_code"});
        let diagnostic = sanitize_provider_diagnostic(&value);
        assert!(diagnostic.is_none());
    }

    #[test]
    fn extra_fields_are_discarded_when_sanitized() {
        let value = json!({"category":"unknown", "httpStatus":429,
            "evidence":"structured_status", "secret":"never expose"});
        let diagnostic = sanitize_provider_diagnostic(&value).expect("valid status");
        assert_eq!(
            serde_json::to_value(diagnostic).expect("serialize"),
            json!({"category":"unknown", "httpStatus":429, "evidence":"structured_status"})
        );
    }

    #[test]
    fn siwc_route_round_trips_as_a_separate_wire_field() {
        for (value, evidence) in [
            (
                json!({"category":"unknown", "httpStatus":403, "evidence":"structured_status", "route":"siwc"}),
                ProviderDiagnosticEvidence::SiwcStructuredStatus,
            ),
            (
                json!({"category":"invalid_request", "httpStatus":403, "code":"chatpass_v2_scope_not_authorized",
                    "evidence":"structured_code", "route":"siwc"}),
                ProviderDiagnosticEvidence::SiwcStructuredCode,
            ),
        ] {
            let diagnostic = sanitize_provider_diagnostic(&value).expect("SIWC diagnostic");
            assert_eq!(diagnostic.route(), Some(ProviderDiagnosticRoute::Siwc));
            assert_eq!(diagnostic.evidence, evidence);
            assert_eq!(serde_json::to_value(diagnostic).expect("serialize"), value);
        }
        assert!(sanitize_provider_diagnostic(&json!({
            "category":"unknown", "httpStatus":403, "evidence":"structured_status", "route":"api_key"
        })).is_none());
    }
}
