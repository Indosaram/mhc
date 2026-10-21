use std::error::Error;
use std::fmt;

use crate::types::ProviderDiagnostic;

/// Classify a native transport failure before its typed source is formatted away.
/// Local hooks never call this function.
pub fn diagnostic_from_transport(error: &(dyn Error + 'static)) -> Option<ProviderDiagnostic> {
    let mut current = Some(error);
    for _ in 0..4 {
        let source = current?;
        let code = if let Some(error) = source.downcast_ref::<std::io::Error>() {
            match error.kind() {
                std::io::ErrorKind::ConnectionReset => Some("ECONNRESET"),
                std::io::ErrorKind::ConnectionRefused => Some("ECONNREFUSED"),
                std::io::ErrorKind::TimedOut => Some("ETIMEDOUT"),
                std::io::ErrorKind::BrokenPipe => Some("EPIPE"),
                _ => None,
            }
        } else if let Some(error) = source.downcast_ref::<reqwest::Error>() {
            if error.is_timeout() {
                Some("APIConnectionTimeoutError")
            } else if error.is_connect() || error.is_body() {
                Some("APIConnectionError")
            } else {
                None
            }
        } else {
            None
        };
        if let Some(code) = code {
            return crate::types::sanitize_provider_diagnostic(&ProviderDiagnostic {
                category: crate::types::ProviderDiagnosticCategory::Network,
                http_status: None,
                code: Some(code.to_owned()),
                evidence: crate::types::ProviderDiagnosticEvidence::StructuredTransport,
            });
        }
        current = source.source();
    }
    None
}

#[derive(Debug)]
pub struct ProviderDiagnosticCarrier {
    error: Box<dyn Error + Send + Sync>,
    diagnostic: Option<ProviderDiagnostic>,
}

impl fmt::Display for ProviderDiagnosticCarrier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.error, formatter)
    }
}

impl Error for ProviderDiagnosticCarrier {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.error.as_ref())
    }
}

pub fn attach_provider_diagnostic(
    error: impl Error + Send + Sync + 'static,
    diagnostic: Option<ProviderDiagnostic>,
) -> ProviderDiagnosticCarrier {
    ProviderDiagnosticCarrier {
        error: Box::new(error),
        diagnostic,
    }
}

pub fn peek_provider_diagnostic<'a>(
    error: &'a (dyn Error + 'static),
) -> Option<&'a ProviderDiagnostic> {
    error
        .downcast_ref::<ProviderDiagnosticCarrier>()?
        .diagnostic
        .as_ref()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ProviderDiagnosticCategory, ProviderDiagnosticEvidence};

    #[test]
    fn typed_transport_kind_does_not_trust_error_text() {
        // Given identical text on a typed reset and an unclassified local error.
        let reset = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "opaque");
        let local = std::io::Error::other("ECONNRESET");
        // When only the structured kind is inspected.
        let diagnostic = diagnostic_from_transport(&reset).expect("transport diagnostic");
        // Then reset provenance survives while local wording remains unclassified.
        assert_eq!(diagnostic.code.as_deref(), Some("ECONNRESET"));
        assert!(diagnostic_from_transport(&local).is_none());
    }

    #[test]
    fn diagnostic_is_read_only_from_adapter_owned_carrier() {
        let diagnostic = ProviderDiagnostic {
            category: ProviderDiagnosticCategory::Auth,
            http_status: Some(401),
            code: None,
            evidence: ProviderDiagnosticEvidence::StructuredStatus,
        };
        let error =
            attach_provider_diagnostic(std::io::Error::other("failure"), Some(diagnostic.clone()));
        let read = crate::utils::provider_diagnostic::read_provider_diagnostic(&error);
        assert_eq!(read, Some(diagnostic));
        assert_eq!(error.to_string(), "failure");
        assert!(
            crate::utils::provider_diagnostic::read_provider_diagnostic(&std::io::Error::other(
                "providerDiagnostic: auth"
            ))
            .is_none()
        );
    }
}
