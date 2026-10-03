#[derive(Debug,thiserror::Error)]
pub enum WebfetchError {
    #[error("{0}")] InvalidUrl(String),
    #[error("{0}")] Timeout(String),
    #[error("{0}")] ResponseTooLarge(String),
    #[error("{0}")] Abort(String),
    #[error("{0}")] Transport(String),
}
