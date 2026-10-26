use std::fmt;

/// A rejected delivery. Only malformed or oversized events cannot succeed on retry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelemetryDeliveryError {
    pub permanent: bool,
    pub status: Option<u16>,
    pub cause: Option<crate::types::TelemetryError>,
}

impl TelemetryDeliveryError {
    pub fn new(status: Option<u16>, cause: Option<crate::types::TelemetryError>) -> Self {
        Self {
            permanent: matches!(status, Some(400 | 413)),
            status,
            cause,
        }
    }
}

impl fmt::Display for TelemetryDeliveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.status {
            Some(status) => write!(f, "telemetry event was rejected with HTTP {status}"),
            None => f.write_str("telemetry event was not delivered (no response)"),
        }
    }
}

impl std::error::Error for TelemetryDeliveryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause
            .as_ref()
            .map(|cause| cause as &(dyn std::error::Error + 'static))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_400_and_413_are_permanent() {
        for status in [
            None,
            Some(400),
            Some(401),
            Some(403),
            Some(408),
            Some(413),
            Some(429),
            Some(500),
            Some(503),
        ] {
            let error = TelemetryDeliveryError::new(status, None);
            assert_eq!(error.permanent, matches!(status, Some(400 | 413)));
            assert_eq!(error.status, status);
        }
    }
}
