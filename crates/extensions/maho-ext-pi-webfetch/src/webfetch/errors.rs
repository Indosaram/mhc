#[derive(Debug,thiserror::Error)]
pub enum WebfetchError {
    #[error("{0}")]
    InvalidUrl(String),
    #[error("Request aborted")]
    Aborted,
    #[error("Request timed out after {0}s")]
    Timeout(u64),
    #[error("Response too large (exceeds 5MB limit)")]
    ResponseTooLarge,
    #[error(transparent)]
    Network(#[from] reqwest::Error),
}
