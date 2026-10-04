use crate::{affinity::AllAccountsBlockedError, errors::{SdkErrorKind, classify_sdk_error}};

pub fn with_auth_guidance(error: &anyhow::Error) -> String {
    let message = error.to_string();
    if let Some(blocked) = error.downcast_ref::<AllAccountsBlockedError>() {
        return if blocked.block_reason == Some("auth_error") {
            "All Claude accounts for anthropic-subscription are currently blocked (authentication error).\n  /login anthropic-subscription - re-authenticate to refresh the blocked account\n  /claude-account list - inspect account states".into()
        } else {
            format!("{message}\n  /claude-account list - inspect account states\n  /login anthropic-subscription - add another account")
        };
    }
    let hint = match classify_sdk_error(&serde_json::Value::String(message.clone())).kind {
        SdkErrorKind::OrgNotAllowed => Some("This organization's policy disallows subscription OAuth use here. Use an API key (ANTHROPIC_API_KEY) or an account from an allowed organization."),
        SdkErrorKind::Billing => Some("The selected Claude account has a billing problem. Check the plan at claude.com or switch accounts with /claude-account pin <name>."),
        SdkErrorKind::AuthError => Some("The account's OAuth token was rejected. Re-run /login anthropic-subscription to refresh it, or remove the account with /claude-account remove <name>."),
        SdkErrorKind::Entitlement => Some("This model needs usage credits on the selected Claude account (it is not included in the subscription). Switch models with /model, enable usage credits at claude.com, or pick another account with /claude-account pin <name>."),
        _ => None,
    };
    match hint { Some(hint) => format!("{message}\n{hint}"), None => message }
}
