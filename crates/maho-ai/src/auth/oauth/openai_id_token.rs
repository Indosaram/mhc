//! Port of senpi packages/ai/src/auth/oauth/openai-id-token.ts.

use super::transport::{HttpRequest, OAuthTransport};
use crate::utils::abort::AbortSignal;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::signature::{RSA_PKCS1_2048_8192_SHA256, RsaPublicKeyComponents};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use tokio::sync::Mutex;
use url::Url;

const ISSUER: &str = "https://auth.openai.com";
pub(super) const DISCOVERY_URL: &str = "https://auth.openai.com/.well-known/openid-configuration";
const CACHE_MS: f64 = 300_000.0;

#[derive(Debug, PartialEq, Eq)]
pub struct OpenAIIdentity {
    pub subject: String,
    /// Display metadata only; never an account identity or workspace identifier.
    pub email: Option<String>,
    pub issuer: &'static str,
}

#[derive(Debug, thiserror::Error)]
#[error("OpenAI ID token validation failed: {0}")]
pub struct OpenAIIdTokenValidationError(&'static str);

#[derive(Default)]
struct Cache {
    discovery: Option<(Url, f64)>,
    jwks: Option<(Vec<Map<String, Value>>, f64)>,
}

#[derive(Default)]
pub struct OpenAIIdTokenVerifier {
    cache: Mutex<Cache>,
}

type Validation<T> = Result<T, OpenAIIdTokenValidationError>;

fn decode(value: &str) -> Validation<Vec<u8>> {
    if value.is_empty() {
        return Err(OpenAIIdTokenValidationError("invalid JWT encoding"));
    }
    URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| OpenAIIdTokenValidationError("invalid JWT encoding"))
}

async fn fetch_json<T: DeserializeOwned>(
    transport: &dyn OAuthTransport,
    url: &str,
    signal: &AbortSignal,
    error: &'static str,
) -> Validation<T> {
    let response = transport
        .execute(
            HttpRequest {
                method: "GET".into(),
                url: url.into(),
                headers: Vec::new(),
                body: None,
                timeout_ms: None,
            },
            signal,
        )
        .await
        .map_err(|_| OpenAIIdTokenValidationError(error))?;
    if !response.ok() {
        return Err(OpenAIIdTokenValidationError(error));
    }
    serde_json::from_str(&response.body)
        .map_err(|_| OpenAIIdTokenValidationError("could not verify ID token"))
}

/// Discovery supplies only HTTPS endpoints without embedded credentials.
pub(super) fn https_endpoint(endpoint: &str) -> Option<Url> {
    let url = Url::parse(endpoint).ok()?;
    (url.scheme() == "https" && url.username().is_empty() && url.password().is_none())
        .then_some(url)
}

impl Cache {
    async fn load_keys(
        &mut self,
        transport: &dyn OAuthTransport,
        signal: &AbortSignal,
        force: bool,
    ) -> Validation<&[Map<String, Value>]> {
        if self
            .discovery
            .as_ref()
            .is_none_or(|(_, expires)| *expires <= transport.now_ms())
        {
            let config: Map<String, Value> =
                fetch_json(transport, DISCOVERY_URL, signal, "discovery request failed").await?;
            let uri = config
                .get("jwks_uri")
                .and_then(Value::as_str)
                .and_then(https_endpoint)
                .ok_or(OpenAIIdTokenValidationError("invalid discovery JWKS URI"))?;
            self.jwks = None;
            self.discovery = Some((uri, transport.now_ms() + CACHE_MS));
        }
        if force
            || self
                .jwks
                .as_ref()
                .is_none_or(|(_, expires)| *expires <= transport.now_ms())
        {
            #[derive(serde::Deserialize)]
            struct Jwks {
                keys: Vec<Map<String, Value>>,
            }
            let (uri, _) = self
                .discovery
                .as_ref()
                .ok_or(OpenAIIdTokenValidationError("invalid discovery JWKS URI"))?;
            let data: Jwks =
                fetch_json(transport, uri.as_str(), signal, "JWKS request failed").await?;
            self.jwks = Some((data.keys, transport.now_ms() + CACHE_MS));
        }
        self.jwks
            .as_ref()
            .map(|(keys, _)| keys.as_slice())
            .ok_or(OpenAIIdTokenValidationError("invalid JWKS"))
    }
}

impl OpenAIIdTokenVerifier {
    /// Verifies RS256 signature and the claims bound to this authorization request.
    pub async fn verify(
        &self,
        transport: &dyn OAuthTransport,
        id_token: Option<&str>,
        client_id: &str,
        nonce: &str,
        signal: &AbortSignal,
    ) -> Validation<OpenAIIdentity> {
        let token = id_token.ok_or(OpenAIIdTokenValidationError("missing ID token"))?;
        let parts = token.split('.').collect::<Vec<_>>();
        let [header_part, payload_part, signature_part] = parts.as_slice() else {
            return Err(OpenAIIdTokenValidationError("invalid JWT"));
        };
        let header: Value = serde_json::from_slice(&decode(header_part)?)
            .map_err(|_| OpenAIIdTokenValidationError("could not verify ID token"))?;
        let kid = header["kid"]
            .as_str()
            .filter(|kid| !kid.is_empty())
            .filter(|_| header["alg"] == "RS256" && header.get("crit").is_none())
            .ok_or(OpenAIIdTokenValidationError("unsupported signing header"))?;
        let matches = |key: &&Map<String, Value>| {
            key.get("kid").and_then(Value::as_str) == Some(kid)
                && key.get("kty").and_then(Value::as_str) == Some("RSA")
                && key.get("alg").is_none_or(|v| v == "RS256")
                && key.get("use").is_none_or(|v| v == "sig")
        };
        let (n, e) = {
            let mut cache = self.cache.lock().await;
            let mut keys = cache.load_keys(transport, signal, false).await?;
            if !keys.iter().any(|key| matches(&key)) {
                keys = cache.load_keys(transport, signal, true).await?;
            }
            let mut matching = keys.iter().filter(matches);
            let key = matching
                .next()
                .filter(|_| matching.next().is_none())
                .ok_or(OpenAIIdTokenValidationError(
                    "signing key not found or ambiguous",
                ))?;
            if key.get("key_ops").is_some_and(|ops| {
                !ops.as_array().is_some_and(|ops| {
                    ops.iter().all(Value::is_string) && ops.iter().any(|op| op == "verify")
                })
            }) {
                return Err(OpenAIIdTokenValidationError("could not verify ID token"));
            }
            let component = |name| {
                key.get(name)
                    .and_then(Value::as_str)
                    .ok_or(OpenAIIdTokenValidationError("could not verify ID token"))
                    .and_then(decode)
            };
            (component("n")?, component("e")?)
        };
        let signed_length = header_part.len() + 1 + payload_part.len();
        RsaPublicKeyComponents { n, e }
            .verify(
                &RSA_PKCS1_2048_8192_SHA256,
                &token.as_bytes()[..signed_length],
                &decode(signature_part)?,
            )
            .map_err(|_| OpenAIIdTokenValidationError("invalid signature"))?;
        let claims: Value = serde_json::from_slice(&decode(payload_part)?)
            .map_err(|_| OpenAIIdTokenValidationError("could not verify ID token"))?;
        if claims["iss"] != ISSUER {
            return Err(OpenAIIdTokenValidationError("issuer mismatch"));
        }
        let audience = &claims["aud"];
        if audience.as_str() != Some(client_id)
            && !audience.as_array().is_some_and(|values| {
                values.iter().all(Value::is_string) && values.iter().any(|v| v == client_id)
            })
        {
            return Err(OpenAIIdTokenValidationError("audience mismatch"));
        }
        if claims["exp"]
            .as_f64()
            .is_none_or(|exp| !exp.is_finite() || exp <= transport.now_ms() / 1000.0)
        {
            return Err(OpenAIIdTokenValidationError(
                "expired or missing expiration",
            ));
        }
        if claims["nonce"] != nonce {
            return Err(OpenAIIdTokenValidationError("nonce mismatch"));
        }
        let subject = claims["sub"]
            .as_str()
            .filter(|subject| !subject.trim().is_empty())
            .ok_or(OpenAIIdTokenValidationError("missing subject"))?;
        Ok(OpenAIIdentity {
            subject: subject.into(),
            issuer: ISSUER,
            email: claims["email"].as_str().map(str::to_owned),
        })
    }
}

#[cfg(test)]
#[path = "openai_id_token_tests.rs"]
pub(super) mod test_support;
