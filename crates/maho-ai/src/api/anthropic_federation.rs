use crate::types::{Model, StreamOptions};
use crate::utils::provider_env::get_provider_env_value;
use serde_json::{Value, json};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Federation {
    base_url: String,
    organization: String,
    rule: String,
    file: String,
    workspace: Option<String>,
    service_account: Option<String>,
}

struct CachedToken {
    config: Federation,
    token: String,
    expires: Instant,
}

static TOKEN: LazyLock<tokio::sync::Mutex<Option<CachedToken>>> =
    LazyLock::new(|| tokio::sync::Mutex::new(None));

pub(super) fn resolve(
    model: &Model,
    options: &StreamOptions,
    has_auth: bool,
) -> Option<Federation> {
    if model.provider != "anthropic" || has_auth {
        return None;
    }
    let env = options.request.env.as_ref();
    let get = |name| get_provider_env_value(name, env).filter(|value| !value.is_empty());
    Some(Federation {
        base_url: model.base_url.trim_end_matches('/').to_owned(),
        organization: get("ANTHROPIC_ORGANIZATION_ID")?,
        rule: get("ANTHROPIC_FEDERATION_RULE_ID")?,
        file: get("ANTHROPIC_IDENTITY_TOKEN_FILE")?,
        workspace: get("ANTHROPIC_WORKSPACE_ID"),
        service_account: get("ANTHROPIC_SERVICE_ACCOUNT_ID"),
    })
}

pub(super) async fn token(
    config: &Federation,
    options: &StreamOptions,
    force: bool,
) -> Result<String, String> {
    let work = async {
        let mut cache = TOKEN.lock().await;
        if !force
            && let Some(cached) = cache.as_ref().filter(|cached| {
                cached.config == *config
                    && cached.expires > Instant::now() + Duration::from_secs(30)
            })
        {
            return Ok(cached.token.clone());
        }
        let url = reqwest::Url::parse(&config.base_url).map_err(|error| error.to_string())?;
        if url.scheme() != "https"
            && !(url.scheme() == "http"
                && matches!(
                    url.host_str(),
                    Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
                ))
        {
            return Err("Refusing to send identity token over a non-HTTPS endpoint".into());
        }
        let assertion = tokio::fs::read_to_string(&config.file)
            .await
            .map_err(|error| format!("Reading Anthropic identity token: {error}"))?;
        let assertion = assertion.trim();
        if assertion.is_empty() || assertion.len() > 16 * 1024 {
            return Err("Anthropic identity token must contain 1 to 16384 bytes".into());
        }
        let mut body = json!({"grant_type":"urn:ietf:params:oauth:grant-type:jwt-bearer", "assertion":assertion,
            "federation_rule_id":config.rule, "organization_id":config.organization});
        if let Some(value) = &config.workspace {
            body["workspace_id"] = json!(value);
        }
        if let Some(value) = &config.service_account {
            body["service_account_id"] = json!(value);
        }
        let client = options.request.fetch.clone().unwrap_or(
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|error| error.to_string())?,
        );
        let response = client
            .post(format!("{}/v1/oauth/token", config.base_url))
            .header(
                "anthropic-beta",
                "oauth-2025-04-20,oidc-federation-2026-04-01",
            )
            .json(&body)
            .send()
            .await
            .map_err(|error| format!("Anthropic token exchange: {error}"))?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!(
                "Anthropic token exchange failed with status {status}"
            ));
        }
        if response
            .content_length()
            .is_some_and(|length| length > 1024 * 1024)
        {
            return Err("Anthropic token response exceeds 1 MiB".into());
        }
        let mut response = response;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|error| error.to_string())? {
            if bytes.len() + chunk.len() > 1024 * 1024 {
                return Err("Anthropic token response exceeds 1 MiB".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let data: Value = serde_json::from_slice(&bytes)
            .map_err(|_| "Anthropic token endpoint returned invalid JSON".to_owned())?;
        let token = data["access_token"]
            .as_str()
            .filter(|token| !token.is_empty())
            .ok_or_else(|| "Anthropic token response missing access_token".to_owned())?
            .to_owned();
        if data["token_type"]
            .as_str()
            .is_some_and(|kind| !kind.eq_ignore_ascii_case("bearer"))
        {
            return Err("Anthropic token response has unsupported token_type".into());
        }
        let seconds = data["expires_in"]
            .as_f64()
            .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
            .ok_or_else(|| "Anthropic token response missing valid expires_in".to_owned())?;
        let duration = Duration::try_from_secs_f64(seconds).map_err(|error| error.to_string())?;
        let expires = Instant::now()
            .checked_add(duration)
            .ok_or_else(|| "Anthropic token expiry out of range".to_owned())?;
        *cache = Some(CachedToken {
            config: config.clone(),
            token: token.clone(),
            expires,
        });
        Ok(token)
    };
    let timed = tokio::time::timeout(
        Duration::from_millis(options.request.timeout_ms.unwrap_or(600_000)),
        work,
    );
    if let Some(signal) = &options.request.signal {
        tokio::select! {
            biased;
            () = signal.cancelled() => Err("Anthropic token exchange aborted".to_owned()),
            result = timed => result.map_err(|error| error.to_string())?,
        }
    } else {
        timed.await.map_err(|error| error.to_string())?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[tokio::test]
    async fn token_is_reused_until_forced_refresh() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let file = directory.path().join("identity.jwt");
        tokio::fs::write(&file, "header.payload.signature").await?;
        let exchanges = Arc::new(AtomicUsize::new(0));
        let count = exchanges.clone();
        let app = axum::Router::new().route(
            "/v1/oauth/token",
            axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
                let count = count.clone();
                async move {
                    assert_eq!(
                        body["grant_type"],
                        "urn:ietf:params:oauth:grant-type:jwt-bearer"
                    );
                    assert_eq!(body["federation_rule_id"], "rule");
                    count.fetch_add(1, Ordering::SeqCst);
                    axum::Json(json!({"access_token":"federated", "expires_in":3600}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let config = Federation {
            base_url: format!("http://{address}"),
            organization: "org".into(),
            rule: "rule".into(),
            file: file.to_string_lossy().into_owned(),
            workspace: None,
            service_account: None,
        };
        let options = StreamOptions::default();
        for _ in 0..3 {
            assert_eq!(
                token(&config, &options, false)
                    .await
                    .map_err(anyhow::Error::msg)?,
                "federated"
            );
        }
        assert_eq!(exchanges.load(Ordering::SeqCst), 1);
        token(&config, &options, true)
            .await
            .map_err(anyhow::Error::msg)?;
        assert_eq!(exchanges.load(Ordering::SeqCst), 2);
        server.abort();
        Ok(())
    }
}
