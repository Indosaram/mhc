use crate::types::{
    ProviderDiagnostic, ProviderDiagnosticCategory as Category,
    ProviderDiagnosticEvidence as Evidence,
};

pub use crate::types::PROVIDER_DIAGNOSTIC_MAX_BYTES;

pub fn category_for_provider_code(code: &str) -> Option<Category> {
    Some(match code {
        "authentication_error" | "invalid_api_key" => Category::Auth,
        "rate_limit_error" | "rate_limit_exceeded" => Category::RateLimit,
        "insufficient_quota" => Category::Quota,
        "context_length_exceeded" => Category::ContextLimit,
        "invalid_request_error" => Category::InvalidRequest,
        "overloaded_error" | "api_error" | "timeout_error" => Category::ProviderUnavailable,
        "billing_error" | "permission_error" | "not_found_error" => Category::Unknown,
        _ => return None,
    })
}

pub fn category_for_transport_code(code: &str) -> Option<Category> {
    let diagnostic = ProviderDiagnostic {
        category: Category::Network,
        http_status: None,
        code: Some(code.to_owned()),
        evidence: Evidence::StructuredTransport,
    };
    crate::types::sanitize_provider_diagnostic(&diagnostic).map(|value| value.category)
}

pub fn category_for_provider_status(status: u16) -> Category {
    match status {
        401 => Category::Auth,
        400 => Category::InvalidRequest,
        500..=599 => Category::ProviderUnavailable,
        _ => Category::Unknown,
    }
}

pub fn is_provider_code_status_compatible(code: &str, status: u16) -> bool {
    let Some(category) = category_for_provider_code(code) else {
        return false;
    };
    crate::types::sanitize_provider_diagnostic(&ProviderDiagnostic {
        category,
        http_status: Some(status),
        code: Some(code.to_owned()),
        evidence: Evidence::StructuredCode,
    })
    .is_some()
}

pub struct ProviderFailureFacts<'a> {
    pub status: Option<&'a serde_json::Value>,
    pub code: Option<&'a str>,
}

pub fn classify_provider_failure(facts: ProviderFailureFacts<'_>) -> Option<ProviderDiagnostic> {
    let status = match facts.status {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => {
            let status = u16::try_from(value.as_u64()?).ok()?;
            if !(400..=599).contains(&status) {
                return None;
            }
            Some(status)
        }
    };
    let diagnostic = match facts.code.and_then(category_for_provider_code) {
        Some(category) => ProviderDiagnostic {
            category,
            http_status: status,
            code: facts.code.map(str::to_owned),
            evidence: Evidence::StructuredCode,
        },
        None => ProviderDiagnostic {
            category: category_for_provider_status(status?),
            http_status: status,
            code: None,
            evidence: Evidence::StructuredStatus,
        },
    };
    crate::types::sanitize_provider_diagnostic(&diagnostic)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unknown_code_falls_back_to_status_without_retaining_raw_code() {
        let status = json!(429);
        let result = classify_provider_failure(ProviderFailureFacts {
            status: Some(&status),
            code: Some("secret-provider-text"),
        })
        .expect("status classification");
        assert_eq!(result.category, Category::Unknown);
        assert_eq!(result.code, None);
        assert_eq!(result.evidence, Evidence::StructuredStatus);
    }

    #[test]
    fn invalid_status_prevents_code_classification() {
        let status = json!(401.5);
        let result = classify_provider_failure(ProviderFailureFacts {
            status: Some(&status),
            code: Some("invalid_api_key"),
        });
        assert!(result.is_none());
    }
}
