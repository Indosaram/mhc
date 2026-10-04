use std::{fmt, path::PathBuf};
#[derive(Debug)]
pub struct InjectionFileReadError { pub path: PathBuf, pub cause: std::io::Error }
impl fmt::Display for InjectionFileReadError {
 fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "Failed to read {}: {}", self.path.display(), self.cause) }
}
impl std::error::Error for InjectionFileReadError {
 fn source(&self) -> Option<&(dyn std::error::Error + 'static)> { Some(&self.cause) }
}
