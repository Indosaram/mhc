#[derive(Debug,thiserror::Error)]
pub enum WebfetchError {
    #[error("{0}")]
    InvalidUrl(String),
    #[error("Request aborted")]
    Aborted,
    #[error("Response too large (exceeds 5MB limit)")]
    ResponseTooLarge,
    #[error(transparent)]
    Network(#[from] reqwest::Error),
}
