//! Signed-in device adapter. Credentials remain in the host's OS credential store.
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use memory_core::sync::keyring_port::DeviceKeyMaterial;
use serde_json::Value;
use std::sync::Arc;

pub const DEFAULT_SERVICE_API: &str = "https://api.omo.dev";
pub type AccountFuture<'a, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, String>> + Send + 'a>>;
pub trait CredentialStore: Send + Sync {
    fn read(&self) -> AccountFuture<'_, Option<Value>>;
    fn write(&self, credentials: Value) -> AccountFuture<'_, ()>;
    fn clear(&self) -> AccountFuture<'_, ()>;
}
/// An unavailable OS keyring is a signed-out device, never a file-backed fallback.
pub struct UnavailableCredentialStore;
impl CredentialStore for UnavailableCredentialStore {
    fn read(&self) -> AccountFuture<'_, Option<Value>> {
        Box::pin(async { Ok(None) })
    }
    fn write(&self, _: Value) -> AccountFuture<'_, ()> {
        Box::pin(async { Err("OS credential store unavailable".into()) })
    }
    fn clear(&self) -> AccountFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
pub struct DeviceAccount {
    pub api: String,
    pub account_id: String,
    pub device_id: String,
    pub material: DeviceKeyMaterial,
    store: Arc<dyn CredentialStore>,
    client: reqwest::Client,
    refresh: tokio::sync::Mutex<()>,
}
impl DeviceAccount {
    pub async fn signed_out(&self) -> Result<bool, String> {
        Ok(self.store.read().await?.is_none())
    }
    pub async fn access_token(&self) -> Result<String, String> {
        let _guard = self.refresh.lock().await;
        access_token(&self.api, &self.client, self.store.as_ref()).await
    }
}
pub type DeviceAccountSource =
    Arc<dyn Fn() -> AccountFuture<'static, Option<Arc<DeviceAccount>>> + Send + Sync>;

pub async fn account_id_of(
    api: &str,
    token: &str,
    client: &reqwest::Client,
) -> Result<String, String> {
    let response = client
        .get(format!("{api}/v1/me"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "memory sync: the account read failed ({})",
            response.status().as_u16()
        ));
    }
    let body: Value = response.json().await.map_err(|error| error.to_string())?;
    body["account"]["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "memory sync: the account read named no account".into())
}
pub fn create_cli_account_source(
    api: Option<&str>,
    env: &std::collections::BTreeMap<String, String>,
    store: Option<Arc<dyn CredentialStore>>,
) -> Result<DeviceAccountSource, String> {
    let configured = api
        .or_else(|| env.get("OMO_SERVICE_API_URL").map(String::as_str))
        .unwrap_or(DEFAULT_SERVICE_API);
    let parsed =
        url::Url::parse(configured).map_err(|error| format!("OMO_SERVICE_API_URL: {error}"))?;
    let loopback = parsed.scheme() == "http"
        && parsed.host_str() == Some("127.0.0.1")
        && parsed.port().is_some();
    if (parsed.scheme() != "https" && !loopback)
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.host_str().is_none()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.path() != "/"
    {
        return Err("OMO_SERVICE_API_URL: expected an HTTPS service origin".into());
    }
    let api = parsed.origin().ascii_serialization();
    let store =
        store.unwrap_or_else(|| super::credential_store::OsCredentialStore::for_origin(&api));
    let client = reqwest::Client::new();
    Ok(Arc::new(move || {
        let (api, store, client) = (api.clone(), store.clone(), client.clone());
        Box::pin(async move {
            let Some(credentials) = store.read().await? else {
                return Ok(None);
            };
            let decode = |key: &str| -> Result<String, String> {
                let value = credentials[key]
                    .as_str()
                    .ok_or_else(|| format!("missing {key}"))?;
                STANDARD
                    .decode(value)
                    .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
                    .map_err(|error| error.to_string())
            };
            let material = DeviceKeyMaterial {
                signing_private_pkcs8: decode("signingPrivateKey")?,
                sealing_private_pkcs8: decode("sealingPrivateKey")?,
            };
            let token = access_token(&api, &client, store.as_ref()).await?;
            let account_id = account_id_of(&api, &token, &client).await?;
            let device_id = credentials["device"]["id"]
                .as_str()
                .ok_or("missing registered device id")?
                .to_owned();
            Ok(Some(Arc::new(DeviceAccount {
                api,
                account_id,
                device_id,
                material,
                store,
                client,
                refresh: tokio::sync::Mutex::new(()),
            })))
        })
    }))
}
async fn access_token(
    api: &str,
    client: &reqwest::Client,
    store: &dyn CredentialStore,
) -> Result<String, String> {
    let mut current = store.read().await?.ok_or("Not signed in. Run omo login.")?;
    if current["refreshState"] == "uncertain" {
        return Err("The refresh outcome is unknown; sign in again".into());
    }
    let expiry = current["accessTokenExpiresAt"]
        .as_str()
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|at| at.timestamp_millis())
        .unwrap_or(0);
    if expiry > chrono::Utc::now().timestamp_millis() + 30_000 {
        return current["accessToken"]
            .as_str()
            .map(str::to_owned)
            .ok_or("missing access token".into());
    }
    let original = current.clone();
    current["refreshState"] = Value::String("uncertain".into());
    store.write(current.clone()).await?;
    let response = client
        .post(format!("{api}/v1/session/refresh"))
        .json(&serde_json::json!({"refreshToken":current["refreshToken"]}))
        .send()
        .await
        .map_err(|_| "The refresh outcome is unknown; sign in again".to_owned())?;
    let status = response.status();
    let body: Value = response.json().await.map_err(|error| error.to_string())?;
    if !status.is_success() {
        let code = body["error"]["code"].as_str().unwrap_or("unknown");
        if status.as_u16() < 500
            && matches!(
                code,
                "unauthorized"
                    | "account_deleted"
                    | "reauth_required"
                    | "invalid_grant"
                    | "device_revoked"
            )
        {
            store.clear().await?;
        } else if status.as_u16() == 429 && code == "rate_limited" {
            store.write(original).await?;
        }
        return Err(format!("memory sync token refresh failed: {code}"));
    }
    let token = body["accessToken"]
        .as_str()
        .ok_or("refresh response has no token")?
        .to_owned();
    current
        .as_object_mut()
        .ok_or("invalid credentials")?
        .remove("refreshState");
    for key in ["accessToken", "accessTokenExpiresAt", "refreshToken"] {
        current[key] = body[key].clone();
    }
    store.write(current).await?;
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn signed_out_store_does_not_read_account() {
        let source = create_cli_account_source(
            Some("http://127.0.0.1:1"),
            &Default::default(),
            Some(Arc::new(UnavailableCredentialStore)),
        )
        .unwrap();
        assert!(source().await.unwrap().is_none());
    }
    #[test]
    fn service_origin_rejects_paths_credentials_and_plain_http() {
        for api in [
            "http://example.com",
            "https://example.com/path",
            "https://user@example.com",
            "https://example.com/?x=1",
        ] {
            assert!(create_cli_account_source(Some(api), &Default::default(), None).is_err());
        }
    }
}
