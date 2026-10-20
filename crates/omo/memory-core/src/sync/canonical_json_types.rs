//! Canonical JSON value and refusal contracts.

pub type CanonicalJsonValue = serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanonicalJsonViolation {
    Json,
    DuplicateKey,
    Number,
    Order,
    Escape,
    Whitespace,
    Encoding,
}

impl CanonicalJsonViolation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::DuplicateKey => "duplicate-key",
            Self::Number => "number",
            Self::Order => "order",
            Self::Escape => "escape",
            Self::Whitespace => "whitespace",
            Self::Encoding => "encoding",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalJsonError {
    pub violation: CanonicalJsonViolation,
    pub message: String,
}

impl CanonicalJsonError {
    pub fn new(violation: CanonicalJsonViolation, message: impl Into<String>) -> Self {
        Self {
            violation,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for CanonicalJsonError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CanonicalJsonError {}
