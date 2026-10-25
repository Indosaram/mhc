use crate::sync::{
    crypto::entry::Head,
    engine::{
        adapters::http_transport::{HttpRequest, HttpTransportOptions},
        error::{EngineError, EngineResult},
    },
    recovery::ports::{AppendResult, KeyringAppendRequest, TransportRefusal},
};
use serde_json::{Value, json};
#[derive(Debug)]
pub struct AccountHttpError {
    pub code: String,
    pub status: u16,
}
impl std::fmt::Display for AccountHttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "account sync request refused: {} ({})",
            self.code, self.status
        )
    }
}
impl std::error::Error for AccountHttpError {}
pub struct AccountState {
    pub head: Head,
    pub current_epoch: u64,
    pub rotation_pending: bool,
}
#[derive(Clone)]
pub struct AccountHttp {
    pub options: HttpTransportOptions,
}
pub fn create_account_http(options: HttpTransportOptions) -> AccountHttp {
    AccountHttp { options }
}
impl AccountHttp {
    pub async fn send(
        &self,
        method: &'static str,
        path: &str,
        body: Option<Value>,
    ) -> EngineResult<(u16, Value)> {
        let token = (self.options.token)().await?;
        let mut headers = vec![("authorization".into(), format!("Bearer {token}"))];
        if body.is_some() {
            headers.push(("content-type".into(), "application/json".into()));
        }
        let response = self
            .options
            .client
            .send(HttpRequest {
                method,
                url: format!("{}{path}", self.options.base_url),
                headers,
                body: body.map(|b| b.to_string().into_bytes()),
            })
            .await?;
        let value = if response.status == 204 {
            Value::Null
        } else if (200..300).contains(&response.status) {
            serde_json::from_slice(&response.body)?
        } else {
            serde_json::from_slice(&response.body).unwrap_or(Value::Null)
        };
        Ok((response.status, value))
    }
    pub fn response_error(status: u16, body: &Value) -> EngineError {
        EngineError::Account(AccountHttpError {
            status,
            code: if status >= 500 {
                "unavailable".into()
            } else {
                body["error"]["code"].as_str().unwrap_or("unknown").into()
            },
        })
    }
    pub async fn read(&self, path: &str) -> EngineResult<Value> {
        let (status, body) = self.send("GET", path, None).await?;
        if !(200..300).contains(&status) {
            return Err(Self::response_error(status, &body));
        }
        Ok(body)
    }
    pub async fn list_entries_after(&self, after: u64) -> EngineResult<Option<Vec<Value>>> {
        Ok(self
            .read(&format!("/v1/sync/entries?after={after}"))
            .await?["entries"]
            .as_array()
            .cloned())
    }
    pub async fn list_keys(&self) -> EngineResult<Option<Vec<Value>>> {
        Ok(self.read("/v1/sync/keys").await?["entries"]
            .as_array()
            .cloned())
    }
    pub async fn list_entries(&self) -> EngineResult<Option<Vec<Value>>> {
        self.list_entries_after(0).await
    }
    pub async fn list_recovery_wraps(&self) -> EngineResult<Option<Vec<Value>>> {
        Ok(self.read("/v1/sync/recovery-wraps").await?["wraps"]
            .as_array()
            .cloned())
    }
    async fn append(
        &self,
        path: &str,
        method: &'static str,
        request: KeyringAppendRequest,
    ) -> EngineResult<AppendResult> {
        let body = json!({"record":request.record,"base_seq":request.base_seq,"link":request.link,"keyring_link":request.keyring_link,"record_sig":request.record_sig,"wraps":request.wraps});
        let (status, body) = self.send(method, path, Some(body)).await?;
        if (200..300).contains(&status) {
            return body["seq"]
                .as_u64()
                .filter(|seq| *seq <= 9_007_199_254_740_991)
                .map(Ok)
                .ok_or_else(|| {
                    EngineError::Account(AccountHttpError {
                        code: "malformed_reply".into(),
                        status,
                    })
                });
        }
        if status >= 500 {
            return Err(Self::response_error(status, &body));
        }
        Ok(Err(match body["error"]["code"].as_str() {
            Some("stale_base") => TransportRefusal::StaleBase,
            Some("rate_limited") => TransportRefusal::RateLimited,
            Some("not_found") => TransportRefusal::NotFound,
            _ => TransportRefusal::Refused,
        }))
    }
    pub async fn append_keys(&self, request: KeyringAppendRequest) -> EngineResult<AppendResult> {
        self.append("/v1/sync/keys", "PUT", request).await
    }
    pub async fn recovery_join(&self, request: KeyringAppendRequest) -> EngineResult<AppendResult> {
        self.append("/v1/sync/recovery/join", "POST", request).await
    }
    pub async fn state(&self) -> EngineResult<Option<AccountState>> {
        let body = self.read("/v1/sync/state").await?;
        Ok((|| {
            Some(AccountState {
                head: Head {
                    seq: body["head"]["seq"].as_u64()?,
                    link: body["head"]["link"].as_str()?.into(),
                    keyring_link: body["head"]["keyring_link"].as_str()?.into(),
                },
                current_epoch: body["current_epoch"].as_u64()?,
                rotation_pending: body["rotation_pending"] == true,
            })
        })())
    }
}

// Recovery's object-safe transport is Send + Sync, whereas the engine's injected HTTP client is
// deliberately local (Rc). Account operations expose the same routes directly without claiming
// cross-thread safety for a local client.
