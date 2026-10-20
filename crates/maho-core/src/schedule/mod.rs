//! Durable scheduled prompts, ported from coding-agent's builtin/schedule modules.
pub mod occurrences;
pub(crate) mod moved_paths;
pub mod runner_lease;
pub mod store;
pub mod types;

#[derive(Debug, thiserror::Error)]
pub enum ScheduleError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Invalid(String),
}
pub type Result<T> = std::result::Result<T, ScheduleError>;

#[cfg(test)]
mod tests;
