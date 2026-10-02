#[derive(Debug)]
pub struct InjectionFileReadError { pub path: std::path::PathBuf, pub cause: std::io::Error }
impl std::fmt::Display for InjectionFileReadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Failed to read {}: {}", self.path.display(), self.cause)
    }
}
impl std::error::Error for InjectionFileReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> { Some(&self.cause) }
}
