pub const QUOTA_COMMAND: &str = "commandcode-quota";
pub const REFRESH_COMMAND: &str = "commandcode-refresh";
pub const STATUS_COMMAND: &str = "commandcode-status";
pub const QUOTA_MISSING_KEY: &str = "Command Code quota requires an API key. Run /login and select Command Code, or set COMMAND_CODE_API_KEY.";

pub fn quota_unavailable_message(snapshot: &crate::quota_types::QuotaSnapshot) -> Option<&str> {
    if snapshot.credits.is_none()
        && snapshot.plan.is_none()
        && snapshot.usage.is_none()
        && snapshot.account.is_none()
    {
        snapshot.warning.as_deref()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn empty_key_returns_warning_without_network() {
        let snapshot = crate::quota::fetch_quota("https://invalid.example", Some("")).await;
        assert_eq!(snapshot.warning.as_deref(), Some(QUOTA_MISSING_KEY));
    }
}
