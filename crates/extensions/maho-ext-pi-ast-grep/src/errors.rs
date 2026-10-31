#[derive(Debug)]
pub enum ProcessError {
    Spawn(std::io::Error),
    Timeout(SearchTimeoutError),
}
impl std::fmt::Display for ProcessError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(error) => error.fmt(formatter),
            Self::Timeout(error) => error.fmt(formatter),
        }
    }
}
impl std::error::Error for ProcessError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn(error) => Some(error),
            Self::Timeout(error) => Some(error),
        }
    }
}

/// Upstream `AstGrepDownloadError` (`name = "AstGrepDownloadError"`).
#[derive(Debug)]
pub struct AstGrepDownloadError {
    pub message: String,
}
impl AstGrepDownloadError {
    pub const NAME: &'static str = "AstGrepDownloadError";
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}
impl std::fmt::Display for AstGrepDownloadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}
impl std::error::Error for AstGrepDownloadError {}

/// Upstream `SearchTimeoutError(timeoutMs)`: message `Search timeout after {timeoutMs}ms`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchTimeoutError {
    pub timeout_ms: u64,
}
impl SearchTimeoutError {
    pub const NAME: &'static str = "SearchTimeoutError";
    pub fn new(timeout_ms: u64) -> Self {
        Self { timeout_ms }
    }
}
impl std::fmt::Display for SearchTimeoutError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Search timeout after {}ms", self.timeout_ms)
    }
}
impl std::error::Error for SearchTimeoutError {}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn search_timeout_message_matches_upstream() {
        assert_eq!(
            SearchTimeoutError::new(200).to_string(),
            "Search timeout after 200ms"
        );
        assert_eq!(
            ProcessError::Timeout(SearchTimeoutError::new(5)).to_string(),
            "Search timeout after 5ms"
        );
    }
    #[test]
    fn timeout_source_is_search_timeout_error() {
        let error = ProcessError::Timeout(SearchTimeoutError::new(7));
        let source = std::error::Error::source(&error).expect("timeout has a source");
        assert_eq!(
            source.downcast_ref::<SearchTimeoutError>(),
            Some(&SearchTimeoutError::new(7))
        );
    }
    #[test]
    fn download_error_keeps_message() {
        assert_eq!(
            AstGrepDownloadError::new("HTTP 404: Not Found").to_string(),
            "HTTP 404: Not Found"
        );
    }
}
