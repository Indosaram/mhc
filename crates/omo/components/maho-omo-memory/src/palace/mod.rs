pub mod client_script;
pub mod collectors;
pub mod command;
pub mod entry_collector;
pub mod generator;
pub mod history_collector;
pub mod people;
pub mod reflection_collector;
pub mod styles;
pub mod template;

#[cfg(test)]
pub mod test_support;

#[derive(Debug)]
pub enum PalaceError {
    Io(std::io::Error),
    Git(memory_core::git::GitError),
    Json(serde_json::Error),
    Message(String),
}

impl std::fmt::Display for PalaceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Git(error) => write!(formatter, "{error}"),
            Self::Json(error) => write!(formatter, "{error}"),
            Self::Message(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for PalaceError {}

impl From<std::io::Error> for PalaceError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<memory_core::git::GitError> for PalaceError {
    fn from(error: memory_core::git::GitError) -> Self {
        Self::Git(error)
    }
}

impl From<serde_json::Error> for PalaceError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<String> for PalaceError {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}

impl From<&str> for PalaceError {
    fn from(message: &str) -> Self {
        Self::Message(message.to_string())
    }
}
