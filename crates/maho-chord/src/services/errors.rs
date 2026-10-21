//! Port of `src/services/errors.ts`.
use serde::{Deserialize, Serialize};
use std::{any::Any, sync::{Arc, Mutex}};

#[derive(Clone, Debug)]
pub struct SharedServiceError(pub Arc<dyn std::error::Error + Send + Sync>);
impl std::fmt::Display for SharedServiceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { std::fmt::Display::fmt(&self.0, formatter) }
}
impl std::error::Error for SharedServiceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> { Some(self.0.as_ref()) }
}
pub(crate) fn share(error: crate::types::ServiceError) -> SharedServiceError {
    match error.downcast::<SharedServiceError>() { Ok(error) => *error, Err(error) => SharedServiceError(Arc::from(error)) }
}
pub struct ServiceCallbackPanic {
    pub operation: &'static str,
    pub payload: Mutex<Box<dyn Any + Send>>,
}
impl std::fmt::Debug for ServiceCallbackPanic {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { formatter.debug_struct("ServiceCallbackPanic").field("operation", &self.operation).finish_non_exhaustive() }
}
impl std::fmt::Display for ServiceCallbackPanic {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(formatter, "{} panicked", self.operation) }
}
impl std::error::Error for ServiceCallbackPanic {}
pub(crate) fn callback_panic(operation: &'static str, payload: Box<dyn Any + Send>) -> crate::types::ServiceError {
    match payload.downcast::<crate::types::ServiceError>() {
        Ok(error) => *error,
        Err(payload) => Box::new(ServiceCallbackPanic { operation, payload: Mutex::new(payload) }),
    }
}
pub const REMOTE_SERVICE_ERROR_CODES: [&str; 8] = [
    "service_not_allowed",
    "service_not_found",
    "service_mode_mismatch",
    "service_member_not_found",
    "service_member_mismatch",
    "service_instance_not_found",
    "service_stale_instance",
    "service_invalid_value",
];
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteServiceErrorCode {
    ServiceNotAllowed,
    ServiceNotFound,
    ServiceModeMismatch,
    ServiceMemberNotFound,
    ServiceMemberMismatch,
    ServiceInstanceNotFound,
    ServiceStaleInstance,
    ServiceInvalidValue,
}
pub fn is_remote_service_error_code(value: &str) -> bool {
    REMOTE_SERVICE_ERROR_CODES.contains(&value)
}
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct RemoteServiceError {
    pub code: RemoteServiceErrorCode,
    pub message: String,
}
impl RemoteServiceError {
    pub fn new(code: RemoteServiceErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ContractError(pub String);
pub(crate) fn contract(message: impl Into<String>) -> crate::types::ServiceError {
    Box::new(ContractError(message.into()))
}
#[derive(Debug, thiserror::Error)]
#[error("{message}: {errors:?}")]
pub struct CollectedErrors {
    pub message: &'static str,
    pub errors: Vec<crate::types::ServiceError>,
}
pub(crate) fn collected(
    mut errors: Vec<crate::types::ServiceError>,
    message: &'static str,
) -> Result<(), crate::types::ServiceError> {
    if errors.len() == 1 {
        return Err(errors.remove(0));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(Box::new(CollectedErrors { message, errors }))
    }
}
