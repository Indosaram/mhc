use thiserror::Error;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ClientError {
    #[error("{message}")]
    Server { code: String, message: String },
    #[error("{0}")]
    Disconnected(String),
    #[error("Client is disposed")]
    Disposed,
    #[error("{0}")]
    Protocol(String),
    #[error("{0}")]
    InvalidOptions(String),
}

impl From<crate::protocol::codec::ProtocolValidationError> for ClientError {
    fn from(value: crate::protocol::codec::ProtocolValidationError) -> Self {
        Self::Protocol(value.to_string())
    }
}
impl From<std::io::Error> for ClientError {
    fn from(value: std::io::Error) -> Self {
        Self::Disconnected(value.to_string())
    }
}
