//! Port of senpi packages/ai/src/utils/chatgpt-subscription-auth.ts.

use base64::Engine;
use serde_json::Value;

/// The JWT claim holding the ChatGPT account that owns a Codex credential.
pub const CHATGPT_SUBSCRIPTION_AUTH_CLAIM_PATH: &str = "https://api.openai.com/auth";

/// Reads the stable ChatGPT account identifier from a Codex access token.
pub fn extract_chatgpt_subscription_account_id(token: &str) -> Option<String> {
    let parts: Vec<&str> = token.split('.').collect();
    let encoded = if parts.len() == 3 { parts[1] } else { return None };
    if encoded.is_empty() {
        return None;
    }
    let standard = encoded.replace('-', "+").replace('_', "/");
    let padded = format!("{standard}{}", "=".repeat((4 - standard.len() % 4) % 4));
    let bytes = base64::engine::general_purpose::STANDARD.decode(padded).ok()?;
    let latin1: String = bytes.iter().map(|b| char::from(*b)).collect();
    let payload: Value = serde_json::from_str(&latin1).ok()?;
    let account = payload.get(CHATGPT_SUBSCRIPTION_AUTH_CLAIM_PATH)?.as_object()?.get("chatgpt_account_id")?;
    account.as_str().filter(|id| !id.is_empty()).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(payload: &str) -> String {
        let body = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload);
        format!("h.{body}.s")
    }

    #[test]
    fn extracts_account_id_or_none() {
        let good = token(r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"acct_1"}}"#);
        assert_eq!(extract_chatgpt_subscription_account_id(&good), Some("acct_1".into()));
        assert_eq!(extract_chatgpt_subscription_account_id(&token(r#"{"https://api.openai.com/auth":{"chatgpt_account_id":""}}"#)), None);
        assert_eq!(extract_chatgpt_subscription_account_id("a.b"), None);
        assert_eq!(extract_chatgpt_subscription_account_id("a.!!!.c"), None);
    }
}
