#[derive(Debug)]
pub enum ProcessError { Spawn(std::io::Error), Timeout(u64) }
impl std::fmt::Display for ProcessError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self { Self::Spawn(error) => error.fmt(formatter), Self::Timeout(ms) => write!(formatter, "Search timeout after {ms}ms") }
    }
}
impl std::error::Error for ProcessError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> { match self { Self::Spawn(error) => Some(error), Self::Timeout(_) => None } }
}
