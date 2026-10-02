use aes_gcm::{Aes256Gcm, KeyInit, aead::{Aead, Payload}};
use base64::{Engine, engine::general_purpose::STANDARD};
use ed25519_dalek::{SigningKey, Signer, pkcs8::DecodePrivateKey};
use hmac::{Hmac, Mac};
use sha2::{Sha256, Digest};
use maho_ext_api::ExtensionFailure;
use std::{collections::BTreeMap, sync::{Mutex, OnceLock}, time::Duration};
use serde_json::{Value, json};

pub const SIGNING_GATE_URL: &str = "https://zcode.z.ai/api/v1/agent/configs";
pub const SIGNING_HANDSHAKE_URL: &str = "https://api.z.ai/api/paas/c1f3a7e2/v2/client";
pub fn session_id() -> &'static str { static ID: OnceLock<String> = OnceLock::new(); ID.get_or_init(|| uuid::Uuid::new_v4().to_string()) }
pub fn parse_api_key(key: &str) -> Option<(&str, &str)> { let (id, secret) = key.split_once('.')?; (!id.is_empty() && !secret.is_empty()).then_some((id, secret)) }
fn derive(secret: &str, info: &[u8]) -> Result<[u8; 32], ExtensionFailure> {
    let mut key = [0; 32];
    hkdf::Hkdf::<Sha256>::new(Some(b"WD_CLIENT_SIGN_KDF_SALT"), secret.as_bytes()).expand(info, &mut key).map_err(|_| ExtensionFailure::new("glm-zcode: key derivation failed"))?;
    Ok(key)
}
pub fn handshake_signature(id: &str, secret: &str, ts: &str, nonce: &str) -> Result<String, ExtensionFailure> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&derive(secret, b"getSignKey_hmac")?).map_err(|_| ExtensionFailure::new("glm-zcode: invalid HMAC key"))?;
    mac.update(format!("get_sign_key\n{id}\n{ts}\n{nonce}").as_bytes());
    Ok(STANDARD.encode(mac.finalize().into_bytes()))
}
pub fn decrypt_private_key(id: &str, secret: &str, cipher: &str) -> Result<SigningKey, ExtensionFailure> {
    let cipher = STANDARD.decode(cipher).map_err(|_| ExtensionFailure::new("glm-zcode: invalid privateCipher base64"))?;
    if cipher.len() <= 28 { return Err(ExtensionFailure::new("glm-zcode: privateCipher is too short")); }
    let key = Aes256Gcm::new_from_slice(&derive(secret, b"ed25519_priv")?).map_err(|_| ExtensionFailure::new("glm-zcode: invalid AES key"))?;
    let plaintext = key.decrypt(aes_gcm::Nonce::from_slice(&cipher[..12]), Payload { msg: &cipher[12..], aad: id.as_bytes() }).map_err(|_| ExtensionFailure::new("glm-zcode: privateCipher decryption failed"))?;
    let der = STANDARD.decode(String::from_utf8_lossy(&plaintext).as_bytes()).map_err(|_| ExtensionFailure::new("glm-zcode: invalid private key base64"))?;
    SigningKey::from_pkcs8_der(&der).map_err(|_| ExtensionFailure::new("glm-zcode: invalid Ed25519 private key"))
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn random_nonce() -> String { hex(&rand::random::<[u8; 16]>()) }
pub fn solve_pow(id: &str, session: &str, ts: &str, nonce: &str) -> Result<String, ExtensionFailure> {
    let challenge = hex(&Sha256::digest(format!("{id}\nzcode\n{session}\n{ts}").as_bytes()));
    for counter in 0..=u32::MAX {
        let candidate = format!("{nonce}{counter:08x}");
        if Sha256::digest(format!("{}\n{candidate}", &challenge[..32]).as_bytes())[0] == 0 { return Ok(candidate); }
    }
    Err(ExtensionFailure::new("glm-zcode: unable to solve client proof of work"))
}
pub fn request_signature(key: &SigningKey, id: &str, ts: &str, version: &str, session: &str, nonce: &str) -> String { STANDARD.encode(key.sign(format!("{id}\n{ts}\n{version}\n{session}\n{nonce}").as_bytes()).to_bytes()) }

#[derive(Default)]
struct State { checked: Option<f64>, key: Option<SigningKey> }
pub struct Signing { states: Mutex<BTreeMap<String, std::sync::Arc<tokio::sync::Mutex<State>>>>, gate: String, handshake: String }
impl Default for Signing { fn default() -> Self { Self { states: Mutex::default(), gate: SIGNING_GATE_URL.into(), handshake: SIGNING_HANDSHAKE_URL.into() } } }
impl Signing {
    async fn resolve_inner(&self, headers: &BTreeMap<String, Option<String>>) -> Result<BTreeMap<String, String>, ExtensionFailure> {
        if headers.get("X-ZCode-Agent").and_then(|value| value.as_deref()) != Some("glm") { return Ok(BTreeMap::new()); }
        let authorization = headers.get("Authorization").and_then(|value| value.as_deref()).unwrap_or_default().trim();
        let mut words = authorization.split_whitespace();
        if !words.next().is_some_and(|word| word.eq_ignore_ascii_case("Bearer")) { return Ok(BTreeMap::new()); }
        let Some(api_key) = words.next().filter(|_| words.next().is_none()) else { return Ok(BTreeMap::new()); };
        let Some((id, secret)) = parse_api_key(api_key) else { return Ok(BTreeMap::new()); };
        let state = self.states.lock().unwrap_or_else(std::sync::PoisonError::into_inner).entry(api_key.into()).or_default().clone();
        let mut state = state.lock().await;
        let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().map_err(|error| ExtensionFailure::new(error.to_string()))?;
        if !state.checked.is_some_and(|checked| crate::oauth::now_ms() - checked < 3_600_000.0) {
            let mut request = client.get(&self.gate).header("x-api-key", api_key).timeout(Duration::from_secs(15));
            for (key, value) in headers {
                if key != "X-ZCode-Agent" && key != "Authorization" && !key.starts_with("x-") && !key.starts_with("X-Client") && let Some(value) = value.as_ref().filter(|value| !value.is_empty()) { request = request.header(key, value); }
            }
            let response = request.send().await.map_err(|error| ExtensionFailure::new(error.to_string()))?;
            if !response.status().is_success() { state.checked = None; return Ok(BTreeMap::new()); }
            let payload: Value = response.json().await.map_err(|error| ExtensionFailure::new(error.to_string()))?;
            if payload.get("code").and_then(Value::as_i64) != Some(0) || payload.pointer("/data/codingPlanSignature/enable") != Some(&Value::Bool(true)) { state.checked = None; return Ok(BTreeMap::new()); }
            state.checked = Some(crate::oauth::now_ms());
        }
        if state.key.is_none() {
            let ts = crate::oauth::now_ms().floor().to_string();
            let nonce = random_nonce();
            let signature = handshake_signature(id, secret, &ts, &nonce)?;
            let response = client.post(&self.handshake).header("Authorization", api_key).json(&json!({"apiKey":api_key,"nonce":nonce,"sig":signature,"ts":ts})).timeout(Duration::from_secs(10)).send().await.map_err(|error| ExtensionFailure::new(error.to_string()))?;
            if response.status().as_u16() != 200 { return Err(ExtensionFailure::new(format!("glm-zcode: signing handshake HTTP status {}", response.status().as_u16()))); }
            let payload: Value = response.json().await.map_err(|error| ExtensionFailure::new(error.to_string()))?;
            if payload.get("code").and_then(Value::as_i64) != Some(200) { return Err(ExtensionFailure::new("glm-zcode: signing handshake was rejected")); }
            let cipher = payload.pointer("/data/privateCipher").and_then(Value::as_str).filter(|value| !value.is_empty()).ok_or_else(|| ExtensionFailure::new("glm-zcode: signing handshake omitted privateCipher"))?;
            state.key = Some(decrypt_private_key(id, secret, cipher)?);
        }
        let key = state.key.as_ref().ok_or_else(|| ExtensionFailure::new("glm-zcode: signing key unavailable"))?;
        let ts = crate::oauth::now_ms().floor().to_string();
        let nonce = random_nonce();
        let pow = solve_pow(id, session_id(), &ts, &nonce)?;
        let version = crate::models::app_version();
        Ok([("X-Client-Ts",ts.clone()),("X-Client-Version",version.clone()),("X-Client-Sig",request_signature(key,id,&ts,&version,session_id(),&nonce)),("X-Session-Id",session_id().into()),("X-Client-Nonce",nonce),("X-App-Id","zcode".into()),("X-Client-Pow",pow),("x-request-id",uuid::Uuid::new_v4().to_string()),("x-zcode-trace-id",uuid::Uuid::new_v4().to_string()),("x-query-id",uuid::Uuid::new_v4().to_string()),("x-zcode-session-type","main".into())].into_iter().map(|(key,value)|(key.into(),value)).collect())
    }
    pub async fn resolve(&self, headers: &BTreeMap<String, Option<String>>) -> BTreeMap<String, String> {
        match self.resolve_inner(headers).await { Ok(headers) => headers, Err(error) => { eprintln!("glm-zcode: client signing unavailable, sending unsigned ({})", crate::oauth::redact_secrets(&error.to_string())); BTreeMap::new() } }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn rejected_handshake_fails_open_and_retries_without_refetching_gate() {
        use std::io::{BufRead,Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1",0)).unwrap(); let base = format!("http://{}",listener.local_addr().unwrap());
        let peer = std::thread::spawn(move || { for path in ["/gate","/handshake","/handshake"] { let (mut stream,_) = listener.accept().unwrap(); stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap(); let mut reader = std::io::BufReader::new(stream.try_clone().unwrap()); let mut line = String::new(); reader.read_line(&mut line).unwrap(); assert!(line.contains(path)); loop { line.clear(); reader.read_line(&mut line).unwrap(); if line == "\r\n" { break; } } let body = if path == "/gate" { json!({"code":0,"data":{"codingPlanSignature":{"enable":true}}}) } else { json!({"code":403}) }.to_string(); write!(stream,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap(); } });
        let signing = Signing { gate: format!("{base}/gate"),handshake: format!("{base}/handshake"), ..Default::default() };
        let headers = BTreeMap::from([("X-ZCode-Agent".into(),Some("glm".into())),("Authorization".into(),Some("Bearer kid123.sec456".into()))]);
        assert!(signing.resolve(&headers).await.is_empty()); assert!(signing.resolve(&headers).await.is_empty()); peer.join().unwrap();
    }
    #[test] fn key_split_preserves_secret_dots() { assert_eq!(parse_api_key("id.secret.more"), Some(("id","secret.more"))); }
    #[test] fn key_split_rejects_missing_halves() { for key in ["", "id", ".secret", "id."] { assert!(parse_api_key(key).is_none()); } }
    #[test] fn pow_satisfies_challenge() { let candidate = solve_pow("id","session","123","00000000000000000000000000000000").unwrap(); let challenge = hex(&Sha256::digest(b"id\nzcode\nsession\n123")); assert_eq!(Sha256::digest(format!("{}\n{candidate}",&challenge[..32]).as_bytes())[0],0); }
    #[test] fn signature_verifies_with_ed25519() { use ed25519_dalek::Verifier; let key = SigningKey::from_bytes(&[7;32]); let encoded = request_signature(&key,"id","123","3.11.2","session","nonce"); let bytes = STANDARD.decode(encoded).unwrap(); let signature = ed25519_dalek::Signature::from_slice(&bytes).unwrap(); key.verifying_key().verify(b"id\n123\n3.11.2\nsession\nnonce",&signature).unwrap(); }
    #[test] fn handshake_signature_matches_reference() { assert_eq!(handshake_signature("id","secret","123","nonce").unwrap(),"VwSjql9oGzxoEltP31cTspmpOjKeYLigrGJ8uobxW/s="); }
    #[test] fn cipher_rejects_short_body() { assert!(decrypt_private_key("id","secret",&STANDARD.encode([0;28])).is_err()); }
    #[tokio::test] async fn foreign_headers_do_not_sign() { assert!(Signing::default().resolve(&BTreeMap::new()).await.is_empty()); }
    fn encrypted_key() -> (String,ed25519_dalek::VerifyingKey) {
        use ed25519_dalek::pkcs8::EncodePrivateKey;
        let key = SigningKey::from_bytes(&[7;32]);
        let der = key.to_pkcs8_der().unwrap();
        let plaintext = STANDARD.encode(der.as_bytes());
        let aes = Aes256Gcm::new_from_slice(&derive("sec456",b"ed25519_priv").unwrap()).unwrap();
        let nonce = [3;12];
        let ciphertext = aes.encrypt(aes_gcm::Nonce::from_slice(&nonce),Payload { msg: plaintext.as_bytes(), aad: b"kid123" }).unwrap();
        (STANDARD.encode([nonce.to_vec(),ciphertext].concat()),key.verifying_key())
    }
    #[test] fn encrypted_server_key_decrypts_and_verifies() {
        use ed25519_dalek::Verifier;
        let (cipher,public) = encrypted_key(); let key = decrypt_private_key("kid123","sec456",&cipher).unwrap();
        public.verify(b"hello",&key.sign(b"hello")).unwrap();
    }
    #[test] fn encrypted_server_key_rejects_wrong_credential() { let (cipher,_) = encrypted_key(); assert!(decrypt_private_key("other","sec456",&cipher).is_err()); }
    #[test] fn process_session_id_is_stable_uuid() { let first = session_id(); assert_eq!(session_id(),first); assert_eq!(uuid::Uuid::parse_str(first).unwrap().get_version_num(),4); }
    #[tokio::test]
    async fn wire_gate_and_handshake_cache_keys_and_sign_fresh_requests() {
        use std::io::{BufRead,Read,Write};
        use ed25519_dalek::Verifier;
        let listener = std::net::TcpListener::bind(("127.0.0.1",0)).unwrap();
        let base = format!("http://{}",listener.local_addr().unwrap());
        let (cipher,public) = encrypted_key();
        let peer = std::thread::spawn(move || {
            for endpoint in ["/gate","/handshake"] {
                let (mut stream,_) = listener.accept().unwrap(); stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap()); let mut line = String::new(); reader.read_line(&mut line).unwrap(); assert!(line.contains(endpoint));
                let mut headers = String::new(); let mut length = 0;
                loop { line.clear(); reader.read_line(&mut line).unwrap(); if line == "\r\n" { break; } let lower = line.to_lowercase(); if let Some(value) = lower.strip_prefix("content-length:") { length = value.trim().parse().unwrap(); } headers.push_str(&lower); }
                let payload = if endpoint == "/gate" {
                    assert!(headers.contains("x-api-key: kid123.sec456")); assert!(!headers.contains("x-zcode-agent:"));
                    json!({"code":0,"data":{"codingPlanSignature":{"enable":true}}})
                } else {
                    assert!(headers.contains("authorization: kid123.sec456")); let mut bytes = vec![0;length]; reader.read_exact(&mut bytes).unwrap(); let body: Value = serde_json::from_slice(&bytes).unwrap();
                    assert_eq!(body["apiKey"],"kid123.sec456"); assert_eq!(body["sig"],handshake_signature("kid123","sec456",body["ts"].as_str().unwrap(),body["nonce"].as_str().unwrap()).unwrap());
                    json!({"code":200,"data":{"privateCipher":cipher}})
                };
                let body = payload.to_string(); write!(stream,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
        });
        let signing = Signing { gate: format!("{base}/gate"), handshake: format!("{base}/handshake"), ..Default::default() };
        let headers = BTreeMap::from([("X-ZCode-Agent".into(),Some("glm".into())),("Authorization".into(),Some("Bearer kid123.sec456".into()))]);
        let first = signing.resolve(&headers).await; peer.join().unwrap();
        let second = signing.resolve(&headers).await;
        assert_ne!(first["X-Client-Nonce"],second["X-Client-Nonce"]);
        assert_eq!(first["X-Session-Id"],second["X-Session-Id"]);
        for headers in [first,second] {
            let message = format!("kid123\n{}\n{}\n{}\n{}",headers["X-Client-Ts"],headers["X-Client-Version"],headers["X-Session-Id"],headers["X-Client-Nonce"]);
            let signature = ed25519_dalek::Signature::from_slice(&STANDARD.decode(&headers["X-Client-Sig"]).unwrap()).unwrap(); public.verify(message.as_bytes(),&signature).unwrap();
        }
    }
    #[tokio::test]
    async fn disabled_gate_is_not_cached_and_never_handshakes() {
        use std::io::{BufRead,Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1",0)).unwrap(); let base = format!("http://{}",listener.local_addr().unwrap());
        let peer = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream,_) = listener.accept().unwrap(); stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap()); let mut line = String::new(); reader.read_line(&mut line).unwrap(); assert!(line.starts_with("GET /gate "));
                loop { line.clear(); reader.read_line(&mut line).unwrap(); if line == "\r\n" { break; } }
                let body = json!({"code":0,"data":{"codingPlanSignature":{"enable":false}}}).to_string(); write!(stream,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
        });
        let signing = Signing { gate: format!("{base}/gate"), handshake: format!("{base}/handshake"), ..Default::default() };
        let headers = BTreeMap::from([("X-ZCode-Agent".into(),Some("glm".into())),("Authorization".into(),Some("Bearer kid123.sec456".into()))]);
        assert!(signing.resolve(&headers).await.is_empty()); assert!(signing.resolve(&headers).await.is_empty()); peer.join().unwrap();
    }
}
