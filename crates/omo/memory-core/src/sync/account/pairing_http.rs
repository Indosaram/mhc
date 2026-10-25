use super::http::{AccountHttp, AccountHttpError};
use crate::sync::{
    crypto::parse::Principal,
    engine::error::{EngineError, EngineResult},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[derive(Clone, Serialize, Deserialize)]
pub struct PairingAnswer {
    pub principal: Principal,
    pub approver_nonce: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct PairingReveal {
    pub principal: Principal,
    pub joining_nonce: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct PendingApproval {
    pub id: String,
    pub account: String,
    pub commitment: String,
    pub answer: Option<PairingAnswer>,
    pub reveal: Option<PairingReveal>,
    pub expires_at: u64,
}
fn parse_pending(value: Value) -> Option<PendingApproval> {
    let parsed: PendingApproval = serde_json::from_value(value).ok()?;
    if parsed
        .answer
        .as_ref()
        .is_some_and(|a| a.principal.kind != "device")
        || parsed
            .reveal
            .as_ref()
            .is_some_and(|r| r.principal.kind != "device")
    {
        return None;
    }
    Some(parsed)
}
pub struct PairingHttp {
    pub http: AccountHttp,
}
pub fn create_pairing_http(http: AccountHttp) -> PairingHttp {
    PairingHttp { http }
}
fn encoded(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}
impl PairingHttp {
    async fn send(
        &self,
        method: &'static str,
        path: &str,
        body: Option<Value>,
    ) -> EngineResult<Value> {
        let (status, value) = self.http.send(method, path, body).await?;
        if !(200..300).contains(&status) {
            return Err(AccountHttp::response_error(status, &value));
        }
        Ok(value)
    }
    async fn pending(
        &self,
        method: &'static str,
        path: &str,
        body: Option<Value>,
    ) -> EngineResult<PendingApproval> {
        parse_pending(self.send(method, path, body).await?).ok_or_else(|| {
            EngineError::Account(AccountHttpError {
                code: "malformed_reply".into(),
                status: 200,
            })
        })
    }
    pub async fn open_pending(
        &self,
        account: &str,
        commitment: &str,
    ) -> EngineResult<PendingApproval> {
        self.pending(
            "PUT",
            "/v1/sync/pending",
            Some(json!({"account":account,"commitment":commitment})),
        )
        .await
    }
    pub async fn read_pending(&self, id: &str) -> EngineResult<Option<PendingApproval>> {
        match self
            .pending("GET", &format!("/v1/sync/pending/{}", encoded(id)), None)
            .await
        {
            Ok(row) => Ok(Some(row)),
            Err(EngineError::Account(e)) if e.code == "not_found" => Ok(None),
            Err(e) => Err(e),
        }
    }
    pub async fn list_pending(&self) -> EngineResult<Vec<PendingApproval>> {
        let body = self.send("GET", "/v1/sync/pending", None).await?;
        Ok(body["pending"]
            .as_array()
            .into_iter()
            .flatten()
            .cloned()
            .filter_map(parse_pending)
            .collect())
    }
    pub async fn answer_pending(
        &self,
        id: &str,
        principal: &Principal,
        nonce: &str,
    ) -> EngineResult<PendingApproval> {
        self.pending(
            "POST",
            &format!("/v1/sync/pending/{}/answer", encoded(id)),
            Some(json!({"principal":principal,"approver_nonce":nonce})),
        )
        .await
    }
    pub async fn reveal_pending(
        &self,
        id: &str,
        principal: &Principal,
        nonce: &str,
    ) -> EngineResult<PendingApproval> {
        self.pending(
            "POST",
            &format!("/v1/sync/pending/{}/reveal", encoded(id)),
            Some(json!({"principal":principal,"joining_nonce":nonce})),
        )
        .await
    }
    pub async fn set_account_sync(&self, enabled: bool) -> EngineResult<()> {
        self.send("PUT", "/v1/account/sync", Some(json!({"enabled":enabled})))
            .await?;
        Ok(())
    }
    pub async fn set_device_sync(&self, id: &str, enabled: bool) -> EngineResult<()> {
        self.send(
            "PUT",
            &format!("/v1/devices/{}/sync", encoded(id)),
            Some(json!({"enabled":enabled})),
        )
        .await?;
        Ok(())
    }
}
