//! Port of senpi packages/ai/src/auth/oauth/devin-token.ts.

use crate::auth::oauth::transport::{HttpRequest, OAuthTransport};
use crate::auth::types::OAuthCredential;
use crate::utils::abort::AbortSignal;
use base64::Engine as _;
use serde_json::{Map, Value};
use std::time::Duration;

pub const DEVIN_TOKEN_URL: &str = "https://api.devin.ai/auth/cli/token";
pub const DEVIN_API_ENDPOINT: &str = "https://api.devin.ai";
pub const DEVIN_FALLBACK_EXPIRY_MS: f64 = 31_536_000_000.0;

const TOKEN_EXCHANGE_TIMEOUT_MS: u64 = 30_000;

fn decode_jwt_expiry(token: &str) -> Option<f64> {
    let payload = token.split('.').nth(1)?;
    if payload.is_empty() {
        return None;
    }
    let mut base64 = payload.replace('-', "+").replace('_', "/");
    let padding = (4 - (base64.len() % 4)) % 4;
    base64.push_str(&"=".repeat(padding));
    let decoded = base64::engine::general_purpose::STANDARD.decode(base64).ok()?;
    let parsed: Value = serde_json::from_slice(&decoded).ok()?;
    let exp = parsed.get("exp")?.as_f64()?;
    if exp.is_finite() && exp > 0.0 {
        return Some((exp * 1000.0).trunc());
    }
    None
}

fn error_detail(body: &Map<String, Value>) -> Option<String> {
    if let Some(Value::String(description)) = body.get("error_description") {
        return Some(description.clone());
    }
    if let Some(Value::String(message)) = body.get("message") {
        return Some(message.clone());
    }
    if let Some(Value::String(error)) = body.get("error") {
        return Some(error.clone());
    }
    if let Some(Value::Object(error)) = body.get("error")
        && let Some(Value::String(message)) = error.get("message") {
            return Some(message.clone());
        }
    None
}

pub fn devin_credential(token: &str, now_ms: f64) -> OAuthCredential {
    let expires = decode_jwt_expiry(token).unwrap_or(now_ms + DEVIN_FALLBACK_EXPIRY_MS);
    OAuthCredential::new(token, token, expires)
}

pub async fn exchange_devin_authorization_code(
    code: &str,
    verifier: &str,
    signal: &AbortSignal,
    transport: &dyn OAuthTransport,
) -> anyhow::Result<OAuthCredential> {
    if signal.aborted() {
        anyhow::bail!("Login cancelled");
    }

    let request = HttpRequest {
        method: "POST".into(),
        url: DEVIN_TOKEN_URL.into(),
        headers: vec![
            ("accept".into(), "application/json".into()),
            ("content-type".into(), "application/json".into()),
        ],
        body: Some(serde_json::json!({ "code": code, "code_verifier": verifier }).to_string()),
        timeout_ms: None,
    };

    let response = tokio::select! {
        biased;
        () = signal.cancelled() => anyhow::bail!("Login cancelled"),
        () = tokio::time::sleep(Duration::from_millis(TOKEN_EXCHANGE_TIMEOUT_MS)) => {
            anyhow::bail!("Devin OAuth token exchange timed out")
        }
        result = transport.execute(request, signal) => match result {
            Ok(response) => response,
            Err(error) => {
                if signal.aborted() {
                    anyhow::bail!("Login cancelled");
                }
                return Err(error);
            }
        },
    };

    let body = match serde_json::from_str::<Value>(&response.body) {
        Ok(Value::Object(object)) => object,
        Ok(_) => Map::new(),
        Err(_) => {
            if response.ok() {
                anyhow::bail!("Devin OAuth returned invalid JSON");
            }
            Map::new()
        }
    };

    if !response.ok() {
        let detail = error_detail(&body);
        let suffix = detail.map(|detail| format!(": {detail}")).unwrap_or_default();
        anyhow::bail!("Devin OAuth token exchange failed (HTTP {}){suffix}", response.status);
    }

    let token = match body.get("token") {
        Some(Value::String(token)) if !token.is_empty() => token.clone(),
        _ => anyhow::bail!("Devin OAuth response carries no \"token\""),
    };

    Ok(devin_credential(&token, transport.now_ms()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::oauth::transport::{ScriptedResponse, ScriptedTransport};
    use crate::utils::abort::AbortController;
    use std::sync::Arc;

    fn jwt_with_exp(exp: f64) -> String {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::json!({ "exp": exp }).to_string());
        format!("header.{payload}.signature")
    }

    fn signal() -> AbortSignal {
        AbortController::new().signal()
    }

    #[test]
    fn credential_reuses_the_token_for_access_and_refresh() {
        let credential = devin_credential("opaque-token", 1_000.0);
        assert_eq!(credential.access, "opaque-token");
        assert_eq!(credential.refresh, "opaque-token");
        assert_eq!(credential.expires, 1_000.0 + DEVIN_FALLBACK_EXPIRY_MS);
    }

    #[test]
    fn credential_reads_the_jwt_expiry_when_present() {
        let credential = devin_credential(&jwt_with_exp(1_700_000_000.0), 1_000.0);
        assert_eq!(credential.expires, 1_700_000_000_000.0);
    }

    #[test]
    fn unreadable_jwt_payload_falls_back_to_the_fixed_window() {
        for token in ["header.!!!not-base64!!!.signature", "no-dots", "header.e30.signature", "header..signature"] {
            let credential = devin_credential(token, 2_000.0);
            assert_eq!(credential.expires, 2_000.0 + DEVIN_FALLBACK_EXPIRY_MS, "{token}");
        }
    }

    #[tokio::test]
    async fn exchanges_the_code_with_a_bare_json_body() {
        let transport = ScriptedTransport::new(vec![(
            None,
            ScriptedResponse::Json { status: 200, body: serde_json::json!({ "token": "jwt-token" }) },
        )]);
        let credential = exchange_devin_authorization_code("code-1", "verifier-1", &signal(), &transport).await.unwrap();
        assert_eq!(credential.access, "jwt-token");

        let requests = transport.requests();
        assert_eq!(requests[0].url, DEVIN_TOKEN_URL);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(
            serde_json::from_str::<Value>(requests[0].body.as_deref().unwrap()).unwrap(),
            serde_json::json!({ "code": "code-1", "code_verifier": "verifier-1" })
        );
    }

    #[tokio::test]
    async fn surfaces_the_error_description_then_message_then_error() {
        let cases = [
            (serde_json::json!({"error_description": "description wins", "message": "m", "error": "e"}), "description wins"),
            (serde_json::json!({"message": "message wins", "error": "e"}), "message wins"),
            (serde_json::json!({"error": "error wins"}), "error wins"),
            (serde_json::json!({"error": {"message": "nested wins"}}), "nested wins"),
            (serde_json::json!({}), ""),
        ];
        for (body, detail) in cases {
            let transport =
                ScriptedTransport::new(vec![(None, ScriptedResponse::Json { status: 400, body: body.clone() })]);
            let error = exchange_devin_authorization_code("c", "v", &signal(), &transport).await.unwrap_err();
            let expected = if detail.is_empty() {
                "Devin OAuth token exchange failed (HTTP 400)".to_string()
            } else {
                format!("Devin OAuth token exchange failed (HTTP 400): {detail}")
            };
            assert_eq!(error.to_string(), expected, "{body}");
        }
    }

    #[tokio::test]
    async fn rejects_a_response_without_a_token() {
        for body in [serde_json::json!({}), serde_json::json!({"token": ""}), serde_json::json!({"token": 5})] {
            let transport = ScriptedTransport::new(vec![(None, ScriptedResponse::Json { status: 200, body })]);
            let error = exchange_devin_authorization_code("c", "v", &signal(), &transport).await.unwrap_err();
            assert_eq!(error.to_string(), "Devin OAuth response carries no \"token\"");
        }
    }

    #[tokio::test]
    async fn invalid_json_is_only_fatal_for_a_successful_response() {
        let transport = ScriptedTransport::new(vec![(None, ScriptedResponse::Text { status: 200, body: "<html>".into() })]);
        let error = exchange_devin_authorization_code("c", "v", &signal(), &transport).await.unwrap_err();
        assert_eq!(error.to_string(), "Devin OAuth returned invalid JSON");

        let transport = ScriptedTransport::new(vec![(None, ScriptedResponse::Text { status: 500, body: "<html>".into() })]);
        let error = exchange_devin_authorization_code("c", "v", &signal(), &transport).await.unwrap_err();
        assert_eq!(error.to_string(), "Devin OAuth token exchange failed (HTTP 500)");
    }

    #[tokio::test]
    async fn refuses_to_start_with_an_aborted_signal() {
        let transport = ScriptedTransport::default();
        let controller = AbortController::new();
        controller.abort(None);
        let error =
            exchange_devin_authorization_code("c", "v", &controller.signal(), &transport).await.unwrap_err();
        assert_eq!(error.to_string(), "Login cancelled");
        assert!(transport.requests().is_empty());
    }

    #[tokio::test]
    async fn cancels_an_in_flight_exchange() {
        let transport = Arc::new(HangingTransport);
        let controller = AbortController::new();
        let signal = controller.signal();
        let exchange = tokio::spawn({
            let transport = transport.clone();
            async move { exchange_devin_authorization_code("c", "v", &signal, transport.as_ref()).await }
        });
        tokio::task::yield_now().await;
        controller.abort(None);
        let error = exchange.await.unwrap().unwrap_err();
        assert_eq!(error.to_string(), "Login cancelled");
    }

    struct HangingTransport;

    #[async_trait::async_trait]
    impl OAuthTransport for HangingTransport {
        async fn execute(
            &self,
            _request: HttpRequest,
            signal: &AbortSignal,
        ) -> anyhow::Result<crate::auth::oauth::transport::HttpResponse> {
            signal.cancelled().await;
            anyhow::bail!("Login cancelled")
        }
    }
}
