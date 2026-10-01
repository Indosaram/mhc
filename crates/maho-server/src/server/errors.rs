use thiserror::Error;

pub const INTERNAL_SERVER_ERROR_MESSAGE: &str = "Internal server error";

#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{message}")]
pub struct ServerError {
    pub code: String,
    pub message: String,
}
impl ServerError {
    pub fn new(code: &str, message: &str) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
    pub fn wrong_server() -> Self {
        Self::new("wrong_server", "Request was addressed to another server")
    }
    pub fn not_attached() -> Self {
        Self::new(
            "session_not_attached",
            "Session is not attached to this client",
        )
    }
    pub fn draining() -> Self {
        Self::new("server_draining", "Server is draining")
    }
}
impl From<std::io::Error> for ServerError {
    fn from(error: std::io::Error) -> Self {
        Self::new("internal_error", &error.to_string())
    }
}
