use maho_ai::{
    oauth::{OAuthAuthInfo, OAuthCredentials, OAuthLoginCallbacks, OAuthPrompt},
    utils::abort::AbortSignal,
};
use maho_ext_api::{ExtensionFailure, ExtensionFuture, ExtensionOAuthConfig};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const STUDIO_BASE_URL: &str = "https://commandcode.ai";
pub const TEN_YEARS_MS: i64 = 10 * 365 * 24 * 60 * 60 * 1000;
const DEFAULT_API_BASE: &str = "https://api.commandcode.ai";
const DEFAULT_AUTH_TIMEOUT_MS: u64 = 120_000;
const LOGIN_CHOICE: &str = "Command Code login: press Enter for browser login, type 'key' to paste an API key, or paste the API key directly:";
const PASTE_KEY: &str = "Paste your Command Code API key:";
const BROWSER_UNAVAILABLE: &str = "Could not start browser auth. Paste your Command Code API key:";
const TRANSFER_FAILED: &str =
    "Automatic transfer failed or timed out. Paste your Command Code API key:";

pub fn sanitize_api_key(raw: &str) -> String {
    raw.replace("\x1b[200~", "")
        .replace("\x1b[201~", "")
        .replace("[200~", "")
        .replace("[201~", "")
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_owned()
}
pub fn credential_from_key(key: &str, now_ms: i64) -> maho_ai::auth::types::OAuthCredential {
    OAuthCredentials::new(key, key, (now_ms + TEN_YEARS_MS) as f64)
}
pub fn auth_timeout_from_env(raw: Option<&str>) -> Duration {
    let value = raw
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_AUTH_TIMEOUT_MS);
    Duration::from_millis(value)
}
pub fn auth_timeout() -> Duration {
    auth_timeout_from_env(std::env::var("COMMANDCODE_AUTH_TIMEOUT_MS").ok().as_deref())
}

pub async fn validate_api_key(api_base: &str, key: &str) -> Result<(), String> {
    let response = reqwest::Client::new()
        .get(format!("{}/alpha/whoami", api_base.trim_end_matches('/')))
        .bearer_auth(key)
        .send()
        .await
        .map_err(|e| format!("Could not validate the Command Code API key: {e}"))?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err("Command Code rejected the API key".into());
    }
    if !status.is_success() {
        let detail = response.text().await.unwrap_or_default();
        return Err(format!(
            "Command Code API error {}: {detail}",
            status.as_u16()
        ));
    }
    Ok(())
}

pub struct CommandCodeOAuth {
    pub api_base: String,
}
impl Default for CommandCodeOAuth {
    fn default() -> Self {
        Self {
            api_base: DEFAULT_API_BASE.into(),
        }
    }
}
impl CommandCodeOAuth {
    pub async fn login(
        &self,
        callbacks: &dyn OAuthLoginCallbacks,
    ) -> Result<OAuthCredentials, String> {
        match self.choose_login_flow(callbacks).await? {
            LoginFlow::ApiKey(key) => self.finish(key).await,
            LoginFlow::Prompt => {
                self.finish(self.prompt_for_key(callbacks, PASTE_KEY).await?)
                    .await
            }
            LoginFlow::Browser => self.browser_login(callbacks).await,
        }
    }

    async fn choose_login_flow(
        &self,
        callbacks: &dyn OAuthLoginCallbacks,
    ) -> Result<LoginFlow, String> {
        let answer = sanitize_api_key(
            &callbacks
                .on_prompt(OAuthPrompt {
                    message: LOGIN_CHOICE.to_owned(),
                    ..Default::default()
                })
                .await,
        );
        let normalized = answer.to_lowercase();
        let normalized = normalized.trim();
        if answer.trim().is_empty() || matches!(normalized, "1" | "b" | "browser") {
            return Ok(LoginFlow::Browser);
        }
        if matches!(normalized, "2" | "k" | "key" | "api" | "paste") {
            return Ok(LoginFlow::Prompt);
        }
        Ok(LoginFlow::ApiKey(answer.trim().to_owned()))
    }

    async fn browser_login(
        &self,
        callbacks: &dyn OAuthLoginCallbacks,
    ) -> Result<OAuthCredentials, String> {
        let Ok(server) = crate::auth_server::CallbackServer::start().await else {
            return self
                .finish(self.prompt_for_key(callbacks, BROWSER_UNAVAILABLE).await?)
                .await;
        };
        callbacks.on_auth(OAuthAuthInfo {
            url: server.auth_url(),
            instructions: None,
        });
        match server.wait(auth_timeout()).await {
            Ok(key) => self.finish(sanitize_api_key(&key)).await,
            Err(_) => {
                self.finish(self.prompt_for_key(callbacks, TRANSFER_FAILED).await?)
                    .await
            }
        }
    }

    async fn prompt_for_key(
        &self,
        callbacks: &dyn OAuthLoginCallbacks,
        message: &str,
    ) -> Result<String, String> {
        let key = sanitize_api_key(
            &callbacks
                .on_prompt(OAuthPrompt {
                    message: message.to_owned(),
                    ..Default::default()
                })
                .await,
        );
        if key.trim().is_empty() {
            return Err("No Command Code API key provided".to_owned());
        }
        Ok(key)
    }

    async fn finish(&self, key: String) -> Result<OAuthCredentials, String> {
        let key = key.trim().to_owned();
        if key.is_empty() {
            return Err("No Command Code API key provided".to_owned());
        }
        validate_api_key(&self.api_base, &key).await?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |t| t.as_millis() as i64);
        Ok(credential_from_key(&key, now))
    }
}

enum LoginFlow {
    Browser,
    Prompt,
    ApiKey(String),
}
impl ExtensionOAuthConfig for CommandCodeOAuth {
    fn name(&self) -> &str {
        "commandcode"
    }
    fn uses_callback_server(&self) -> Option<bool> {
        Some(true)
    }
    fn login<'a>(
        &'a self,
        callbacks: &'a dyn OAuthLoginCallbacks,
    ) -> ExtensionFuture<'a, OAuthCredentials> {
        Box::pin(async move { self.login(callbacks).await.map_err(ExtensionFailure::new) })
    }
    fn refresh_token<'a>(
        &'a self,
        credentials: &'a OAuthCredentials,
        _signal: &'a AbortSignal,
    ) -> ExtensionFuture<'a, OAuthCredentials> {
        Box::pin(async move {
            Ok(credential_from_key(
                &credentials.refresh,
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |t| t.as_millis() as i64),
            ))
        })
    }
    fn get_api_key(&self, credentials: &OAuthCredentials) -> String {
        credentials.access.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sanitizes_terminal_paste() {
        assert_eq!(sanitize_api_key("\x1b[200~abc\x1b[201~\n"), "abc");
    }
    #[test]
    fn credentials_use_ten_year_expiry() {
        let c = credential_from_key("k", 10);
        assert_eq!(c.refresh, "k");
        assert_eq!(c.access, "k");
        assert_eq!(c.expires, (10 + TEN_YEARS_MS) as f64);
    }
    #[test]
    fn timeout_defaults_and_override() {
        assert_eq!(auth_timeout_from_env(None), Duration::from_secs(120));
        assert_eq!(auth_timeout_from_env(Some("7")), Duration::from_millis(7));
    }
}
