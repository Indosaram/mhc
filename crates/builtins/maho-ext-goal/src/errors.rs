//! Typed errors corresponding to the goal store errors and mutation failures.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GoalError {
    #[error("{0}")] Io(String),
    #[error("{0}")] Json(String),
    #[error("{0}")] AlreadyExists(String),
    #[error("{0}")] NotFound(String),
    #[error("{0}")] InvalidStore(String),
    #[error("{0}")] UnsupportedStoreVersion(String),
    #[error("{0}")] InvalidMutation(String),
}
