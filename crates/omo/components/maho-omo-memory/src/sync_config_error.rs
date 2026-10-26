//! Invalid memory sync configuration errors (#10860).
//!
//! A memory sync configuration value the user set is invalid. Thrown from the device account
//! sidecar and recognized in the entry bundle.

use std::fmt;

pub const SYNC_CONFIG_INVALID: &str = "sync-config-invalid";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncConfigError {
    pub variable: String,
    pub message: String,
}

impl fmt::Display for SyncConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.variable, self.message)
    }
}

impl std::error::Error for SyncConfigError {}

impl SyncConfigError {
    pub fn new(variable: impl Into<String>, cause: impl fmt::Display) -> Self {
        let variable = variable.into();
        let message = format!("{variable} is not a valid service URL: {cause}");
        Self { variable, message }
    }

    pub fn code(&self) -> &'static str {
        SYNC_CONFIG_INVALID
    }
}

pub fn is_sync_config_error(error: &(dyn std::error::Error + 'static)) -> bool {
    error.downcast_ref::<SyncConfigError>().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sync_config_error() {
        let err = SyncConfigError::new("SYNC_URL", "invalid scheme");
        assert_eq!(err.code(), SYNC_CONFIG_INVALID);
        assert_eq!(err.variable, "SYNC_URL");
        assert!(err.to_string().contains("invalid scheme"));
        assert!(is_sync_config_error(&err));
    }
}
