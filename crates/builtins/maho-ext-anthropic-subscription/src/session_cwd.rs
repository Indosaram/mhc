//! senpi `session-cwd.ts`: every SDK query and lane-settings lookup must belong to an explicit session.
use std::path::Path;

pub const ANTHROPIC_SUBSCRIPTION_CWD_ERROR: &str = "Anthropic Subscription requires an absolute session cwd; the host working directory is never used.";

/// The session cwd, or the upstream error when it is missing or relative. The host working directory is never a
/// fallback.
pub fn require_anthropic_subscription_cwd(cwd: Option<&Path>) -> anyhow::Result<&Path> {
    match cwd {
        Some(cwd) if !cwd.as_os_str().is_empty() && cwd.is_absolute() => Ok(cwd),
        _ => anyhow::bail!(ANTHROPIC_SUBSCRIPTION_CWD_ERROR),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_empty_and_relative_cwds_are_refused() {
        for cwd in [None, Some(Path::new("")), Some(Path::new("relative/dir"))] {
            assert_eq!(
                require_anthropic_subscription_cwd(cwd)
                    .unwrap_err()
                    .to_string(),
                ANTHROPIC_SUBSCRIPTION_CWD_ERROR
            );
        }
        let absolute = std::env::temp_dir();
        assert_eq!(
            require_anthropic_subscription_cwd(Some(&absolute)).unwrap(),
            absolute.as_path()
        );
    }
}
