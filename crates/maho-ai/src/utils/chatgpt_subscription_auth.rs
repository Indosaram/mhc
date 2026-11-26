//! Port of senpi packages/ai/src/utils/chatgpt-subscription-auth.ts.

use base64::Engine;
use serde_json::Value;

/// The JWT claim holding the ChatGPT account that owns a Codex credential.
pub const CHATGPT_SUBSCRIPTION_AUTH_CLAIM_PATH: &str = "https://api.openai.com/auth";

/// Reads the stable ChatGPT account identifier from a Codex access token.
pub fn extract_chatgpt_subscription_account_id(token: &str) -> Option<String> {
    claim_string(&auth_claims(token)?, "chatgpt_account_id").map(str::to_owned)
}

/// Identifies the member of a workspace, rather than the workspace shared by all its members.
pub fn extract_chatgpt_subscription_identity(token: &str) -> Option<String> {
    let claims = auth_claims(token)?;
    if let Some(member) = claim_string(&claims, "chatgpt_account_user_id") {
        return Some(member.to_owned());
    }
    let user =
        claim_string(&claims, "chatgpt_user_id").or_else(|| claim_string(&claims, "user_id"))?;
    let workspace = claim_string(&claims, "chatgpt_account_id")?;
    Some(format!("{user}/{workspace}"))
}

fn auth_claims(token: &str) -> Option<serde_json::Map<String, Value>> {
    let parts: Vec<&str> = token.split('.').collect();
    let encoded = if parts.len() == 3 {
        parts[1]
    } else {
        return None;
    };
    if encoded.is_empty() {
        return None;
    }
    let standard = encoded.replace('-', "+").replace('_', "/");
    let padded = format!("{standard}{}", "=".repeat((4 - standard.len() % 4) % 4));
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(padded)
        .ok()?;
    let latin1: String = bytes.iter().map(|b| char::from(*b)).collect();
    let mut payload: serde_json::Map<String, Value> = serde_json::from_str(&latin1).ok()?;
    match payload.remove(CHATGPT_SUBSCRIPTION_AUTH_CLAIM_PATH)? {
        Value::Object(claims) => Some(claims),
        _ => None,
    }
}

fn claim_string<'a>(claims: &'a serde_json::Map<String, Value>, key: &str) -> Option<&'a str> {
    claims.get(key)?.as_str().filter(|value| !value.is_empty())
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
        assert_eq!(
            extract_chatgpt_subscription_account_id(&good),
            Some("acct_1".into())
        );
        assert_eq!(
            extract_chatgpt_subscription_account_id(&token(
                r#"{"https://api.openai.com/auth":{"chatgpt_account_id":""}}"#
            )),
            None
        );
        assert_eq!(extract_chatgpt_subscription_account_id("a.b"), None);
        assert_eq!(extract_chatgpt_subscription_account_id("a.!!!.c"), None);
    }

    #[test]
    fn extracts_member_identity_or_scopes_the_user_to_the_workspace() {
        for (claims, expected) in [
            (
                r#""chatgpt_account_user_id":"member","chatgpt_user_id":"user","chatgpt_account_id":"team""#,
                Some("member"),
            ),
            (
                r#""chatgpt_user_id":"user","user_id":"fallback","chatgpt_account_id":"team""#,
                Some("user/team"),
            ),
            (
                r#""chatgpt_user_id":"","user_id":"fallback","chatgpt_account_id":"team""#,
                Some("fallback/team"),
            ),
            (r#""chatgpt_account_id":"team""#, None),
        ] {
            let access = token(&format!(
                r#"{{"{CHATGPT_SUBSCRIPTION_AUTH_CLAIM_PATH}":{{{claims}}}}}"#
            ));
            assert_eq!(
                extract_chatgpt_subscription_identity(&access).as_deref(),
                expected
            );
        }
    }
}
