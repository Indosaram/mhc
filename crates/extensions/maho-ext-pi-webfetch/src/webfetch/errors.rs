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
impl WebfetchError{
    pub const fn name(&self)->&'static str{match self{
        Self::InvalidUrl(_)=>"InvalidWebfetchUrlError",
        Self::Aborted=>"WebfetchAbortError",
        Self::Timeout(_)=>"WebfetchTimeoutError",
        Self::ResponseTooLarge=>"WebfetchResponseTooLargeError",
        Self::Network(_)=>"TypeError",
    }}
}
