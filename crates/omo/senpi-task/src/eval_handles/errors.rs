use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvalHandleErrorCode { Pending, Unsupported, Forbidden, NotFound, Stale, Cancelled, SendRefused }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalHandleHostError { pub code: EvalHandleErrorCode, pub message: String }

impl EvalHandleHostError { pub fn new(code: EvalHandleErrorCode, message: impl Into<String>) -> Self { Self { code, message: message.into() } } }
impl fmt::Display for EvalHandleHostError { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "{:?}: {}", self.code, self.message) } }
impl std::error::Error for EvalHandleHostError {}
