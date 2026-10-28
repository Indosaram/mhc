//! Disable user formatting options for machine-parsed history.
pub const STABLE_LOG_OUTPUT: [&str; 4] =
    ["-c", "log.showSignature=false", "-c", "log.follow=false"];
pub fn stable_log_output() -> Vec<String> {
    STABLE_LOG_OUTPUT
        .iter()
        .map(|arg| (*arg).to_string())
        .collect()
}
