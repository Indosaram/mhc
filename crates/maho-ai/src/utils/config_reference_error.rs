//! Port of senpi packages/ai/src/utils/config-reference-error.ts.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConfigReferenceReason {
    Malformed,
    Forbidden,
    Unset,
    Empty,
    Missing,
    Unreadable,
    Oversize,
    InvalidUtf8,
    NonRegular,
    UnsafeOwner,
    UnsafePermissions,
    Changed,
}

impl ConfigReferenceReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Malformed => "malformed",
            Self::Forbidden => "forbidden",
            Self::Unset => "unset",
            Self::Empty => "empty",
            Self::Missing => "missing",
            Self::Unreadable => "unreadable",
            Self::Oversize => "oversize",
            Self::InvalidUtf8 => "invalid-utf8",
            Self::NonRegular => "non-regular",
            Self::UnsafeOwner => "unsafe-owner",
            Self::UnsafePermissions => "unsafe-permissions",
            Self::Changed => "changed",
        }
    }
}

impl std::fmt::Display for ConfigReferenceReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Contains the declaration, never resolved credential bytes.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{field}: configuration reference {reason}")]
pub struct ConfigReferenceError {
    pub field: String,
    pub reference: Value,
    pub reason: ConfigReferenceReason,
}

impl ConfigReferenceError {
    pub const NAME: &'static str = "ConfigReferenceError";

    pub fn new(field: impl Into<String>, reference: Value, reason: ConfigReferenceReason) -> Self {
        Self {
            field: field.into(),
            reference,
            reason,
        }
    }
}
