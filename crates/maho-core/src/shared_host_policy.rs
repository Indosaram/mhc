//! Port of senpi packages/coding-agent/src/core/shared-host-policy.ts.

/// Shared host is OFF by default; interactive sessions opt in via env flag or the experimental setting.
pub fn should_join_shared_host(app_mode: &str, enable_env: bool, setting_enabled: bool) -> bool {
    if app_mode != "interactive" {
        return false;
    }
    enable_env || setting_enabled
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_interactive_mode_can_join() {
        assert!(!should_join_shared_host("print", true, true));
        assert!(!should_join_shared_host("rpc", true, true));
        assert!(should_join_shared_host("interactive", true, false));
        assert!(should_join_shared_host("interactive", false, true));
        assert!(!should_join_shared_host("interactive", false, false));
    }
}
