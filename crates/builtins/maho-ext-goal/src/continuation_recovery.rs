pub const CONTINUATION_CAP_BLOCKED_REASON: &str = "continuation cap reached";
pub const REPETITION_BLOCKED_REASON: &str = "repeated assistant output";
pub const LENGTH_EXHAUSTED_BLOCKED_REASON: &str = "output truncation repeated";
pub const UNATTENDED_CONTINUATION_BLOCKED_REASON: &str = "unattended continuation limit reached";
pub const PROVIDER_ERROR_BLOCKED_REASON: &str = "provider error ended the turn (retries exhausted)";
pub const CONTEXT_OVERFLOW_BLOCKED_REASON: &str = "context overflow ended the turn (compaction did not recover)";
pub const PROVIDER_AUTH_BLOCKED_REASON_PREFIX: &str = "provider rejected the request: ";
pub fn is_mechanical_continuation_block(reason: Option<&str>) -> bool {
    reason.is_some_and(|reason| reason.starts_with(PROVIDER_AUTH_BLOCKED_REASON_PREFIX) || [CONTINUATION_CAP_BLOCKED_REASON, REPETITION_BLOCKED_REASON, LENGTH_EXHAUSTED_BLOCKED_REASON, UNATTENDED_CONTINUATION_BLOCKED_REASON, PROVIDER_ERROR_BLOCKED_REASON, CONTEXT_OVERFLOW_BLOCKED_REASON].contains(&reason))
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderAuthBlock { pub http_status: u16, pub provider: String, pub model: String }
pub fn provider_auth_blocked_reason(block: &ProviderAuthBlock) -> String {
    let verdict = if block.http_status == 401 { "authentication failed" } else { "access denied" };
    format!("{PROVIDER_AUTH_BLOCKED_REASON_PREFIX}HTTP {} {verdict} for {}/{}", block.http_status, block.provider, block.model)
}
pub fn provider_auth_recovery_hint(block: &ProviderAuthBlock) -> String {
    let fix = if block.http_status == 401 {
        format!("Run /login {} to sign in again, or check the API key or token for {}", block.provider, block.provider)
    } else {
        let plan = if block.provider == "github-copilot" { "your Copilot plan" } else { "your account or plan" };
        format!("Run /login {} to refresh the account, or check that {} is enabled for {}", block.provider, block.model, plan)
    };
    format!("Goal continuation blocked: {}. Retrying would get the same rejection, so the goal stopped instead of retrying. {} and check any proxy or gateway in front of it. Then send any message to resume.", provider_auth_blocked_reason(block), fix)
}
pub fn continuation_cap_recovery_hint(reason: &str) -> String {
    if is_mechanical_continuation_block(Some(reason)) { format!("Goal continuation blocked: {reason}. Send any message to resume.") } else { format!("Goal continuation blocked: {reason}") }
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn runtime_guard_reasons_are_mechanical() { let result = [CONTINUATION_CAP_BLOCKED_REASON, REPETITION_BLOCKED_REASON, LENGTH_EXHAUSTED_BLOCKED_REASON, UNATTENDED_CONTINUATION_BLOCKED_REASON, PROVIDER_ERROR_BLOCKED_REASON, CONTEXT_OVERFLOW_BLOCKED_REASON].map(|reason| is_mechanical_continuation_block(Some(reason))); assert!(result.into_iter().all(|value| value)); }
    #[test] fn unknown_reason_and_absence_are_not_mechanical() { let result = [is_mechanical_continuation_block(None), is_mechanical_continuation_block(Some("waiting for credentials"))]; assert_eq!(result, [false, false]); }
    #[test] fn provider_auth_blocks_are_mechanical_and_include_recovery_guidance() {
        let block=ProviderAuthBlock { http_status:401,provider:"anthropic".into(),model:"claude-test".into() };
        let reason=provider_auth_blocked_reason(&block);
        assert_eq!(reason,"provider rejected the request: HTTP 401 authentication failed for anthropic/claude-test");
        assert!(is_mechanical_continuation_block(Some(&reason)));
        let hint=provider_auth_recovery_hint(&block);
        assert!(hint.contains("/login anthropic")); assert!(hint.contains("Then send any message to resume."));
        let copilot=provider_auth_recovery_hint(&ProviderAuthBlock { http_status:403,provider:"github-copilot".into(),model:"gpt-test".into() });
        assert!(copilot.contains("your Copilot plan"));
    }
}
