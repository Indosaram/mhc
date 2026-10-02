use maho_ai::{oauth::{OAuthAuthInfo, OAuthCredentials, OAuthLoginCallbacks, OAuthPrompt}, utils::abort::AbortSignal};
use maho_ext_api::{ExtensionFailure, ExtensionFuture, ExtensionOAuthConfig};
use serde_json::{Value, json};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const AUTHORIZE_URL: &str = "https://chat.z.ai/api/oauth/authorize";
pub const BROKER_URL: &str = "https://zcode.z.ai/api/v1/oauth/token";
pub const CLI_INIT_URL: &str = "https://zcode.z.ai/api/v1/oauth/cli/init";
pub const CLI_POLL_URL: &str = "https://zcode.z.ai/api/v1/oauth/cli/poll";
pub const ZAI_API_BASE_URL: &str = "https://api.z.ai";
const REDIRECT_URI: &str = "zcode://oauth/callback";
const API_KEY_NAME: &str = "zcode-api-key";
const REQUEST_TIMEOUT_MS: u64 = 30_000;

pub fn now_ms() -> f64 { SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |time| time.as_secs_f64() * 1000.0) }
fn failure(message: impl Into<String>) -> ExtensionFailure { ExtensionFailure::new(message) }
fn data(value: &Value) -> Result<&Value, ExtensionFailure> {
    if !value.is_object() { return Err(failure("GLM ZCode response was not an object")); }
    Ok(value.get("data").filter(|data| data.is_object()).unwrap_or(value))
}
fn select_default(value: Option<&Value>) -> Option<&Value> {
    value.and_then(Value::as_array).and_then(|entries| entries.iter().filter(|entry| entry.is_object()).find(|entry| entry.get("isDefault") == Some(&Value::Bool(true))).or_else(|| entries.iter().find(|entry| entry.is_object())))
}
pub fn redact_secrets(text: &str) -> String {
    let jwt = regex::Regex::new(r"eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+").expect("constant JWT expression");
    let secret = regex::Regex::new(r"[A-Za-z0-9_-]{40,}").expect("constant secret expression");
    secret.replace_all(&jwt.replace_all(text, "[redacted-jwt]"), "[redacted]").into_owned()
}
pub fn callback_code(input: &str, state: &str) -> Result<String, ExtensionFailure> {
    let input = input.trim();
    if input.is_empty() { return Err(failure("GLM ZCode authorization callback URL is required")); }
    let callback = reqwest::Url::parse(input).map_err(|_| failure("GLM ZCode requires the complete zcode:// callback URL"))?;
    if callback.scheme() != "zcode" || callback.host_str() != Some("oauth") || callback.path() != "/callback" || callback.port().is_some() || !callback.username().is_empty() || callback.password().is_some() || callback.fragment().is_some() {
        return Err(failure("GLM ZCode callback URL is invalid"));
    }
    let codes = callback.query_pairs().filter(|(key, _)| key == "code").map(|(_, value)| value.into_owned()).collect::<Vec<_>>();
    let states = callback.query_pairs().filter(|(key, _)| key == "state").map(|(_, value)| value.into_owned()).collect::<Vec<_>>();
    if codes.len() != 1 || states.len() != 1 || codes[0].is_empty() || states[0].is_empty() { return Err(failure("GLM ZCode callback URL must contain exactly one non-empty code and state")); }
    if states[0] != state { return Err(failure("GLM ZCode callback state did not match")); }
    Ok(codes[0].clone())
}

#[derive(Clone)]
pub struct OAuthClient { pub client: reqwest::Client, pub broker: String, pub init: String, pub poll: String, pub api: String }
impl Default for OAuthClient {
    fn default() -> Self { Self { client: reqwest::Client::new(), broker: BROKER_URL.into(), init: CLI_INIT_URL.into(), poll: CLI_POLL_URL.into(), api: ZAI_API_BASE_URL.into() } }
}
impl OAuthClient {
    async fn request(&self, request: reqwest::RequestBuilder, signal: &AbortSignal, label: &str) -> Result<Value, ExtensionFailure> {
        let operation = async {
            let response = request.timeout(Duration::from_millis(REQUEST_TIMEOUT_MS)).send().await.map_err(|error| {
                if error.is_timeout() { failure(format!("GLM ZCode {label} request timed out after {REQUEST_TIMEOUT_MS}ms")) }
                else { failure(format!("GLM ZCode {label} request failed due to a network error ({})", redact_secrets(&error.to_string()))) }
            })?;
            if !response.status().is_success() { return Err(failure(format!("GLM ZCode {label} request failed: {}", response.status().as_u16()))); }
            response.json().await.map_err(|_| failure(format!("GLM ZCode {label} response was not valid JSON")))
        };
        tokio::select! { biased; _ = signal.cancelled() => Err(failure(format!("GLM ZCode {label} request cancelled"))), result = operation => result }
    }
    async fn post(&self, url: &str, body: Value, signal: &AbortSignal, label: &str, token: Option<&str>) -> Result<Value, ExtensionFailure> {
        let mut request = self.client.post(url).header("Accept", "application/json").json(&body);
        if let Some(token) = token.filter(|token| !token.is_empty()) { request = request.bearer_auth(token); }
        self.request(request, signal, label).await
    }
    async fn get(&self, url: &str, signal: &AbortSignal, label: &str, token: &str) -> Result<Value, ExtensionFailure> {
        self.request(self.client.get(url).header("Accept", "application/json").bearer_auth(token), signal, label).await
    }
    pub async fn provision(&self, upstream: &str, signal: &AbortSignal) -> Result<OAuthCredentials, ExtensionFailure> {
        let login = self.post(&format!("{}/api/auth/z/login", self.api), json!({"token":upstream}), signal, "z/login", None).await?;
        let business = data(&login)?.get("access_token").and_then(Value::as_str).filter(|token| !token.is_empty()).ok_or_else(|| failure("GLM ZCode z/login response missing data.access_token"))?;
        let customer = self.get(&format!("{}/api/biz/customer/getCustomerInfo", self.api), signal, "getCustomerInfo", business).await?;
        let customer = data(&customer)?;
        let organization = select_default(customer.get("organizations"));
        let project = select_default(organization.and_then(|value| value.get("projects")));
        let organization_id = organization.and_then(|value| value.get("organizationId")).and_then(Value::as_str);
        let project_id = project.and_then(|value| value.get("projectId")).and_then(Value::as_str);
        let (Some(organization_id), Some(project_id)) = (organization_id, project_id) else { return Err(failure("GLM ZCode getCustomerInfo response missing default organization/project")); };
        let url = format!("{}/api/biz/v1/organization/{organization_id}/projects/{project_id}/api_keys", self.api);
        let listed = self.get(&url, signal, "api_keys.list", business).await?;
        let existing = data(&listed)?.get("data").and_then(Value::as_array).and_then(|keys| keys.iter().find(|key| key.get("name").and_then(Value::as_str) == Some(API_KEY_NAME)));
        let created;
        let key = if let Some(key) = existing { key } else { created = self.post(&url, json!({"name":API_KEY_NAME}), signal, "api_keys.create", Some(business)).await?; data(&created)? };
        let id = key.get("apiKey").filter(|value| value.is_string()).or_else(|| key.get("id")).and_then(Value::as_str).filter(|id| !id.is_empty()).ok_or_else(|| failure("GLM ZCode api_keys response missing apiKey id"))?;
        let mut copy_url = reqwest::Url::parse(&format!("{url}/copy/")).map_err(|error| failure(error.to_string()))?;
        copy_url.path_segments_mut().map_err(|_| failure("invalid API key copy URL"))?.pop_if_empty().push(id);
        let copied = self.get(copy_url.as_str(), signal, "api_keys.copy", business).await?;
        let secret = data(&copied)?.get("secretKey").and_then(Value::as_str).filter(|secret| !secret.is_empty()).ok_or_else(|| failure("GLM ZCode api_keys copy response missing secretKey"))?;
        let mut credential = OAuthCredentials::new(format!("{id}.{secret}"), upstream, now_ms() + 315_360_000_000.0);
        if let Some(email) = customer.get("email").and_then(Value::as_str) { credential.extra.insert("email".into(), json!(email.to_lowercase())); }
        if let Some(id) = customer.get("id").filter(|id| id.is_string() || id.is_number()) { credential.extra.insert("accountId".into(), json!(id.as_str().map(str::to_owned).unwrap_or_else(|| id.to_string()))); }
        Ok(credential)
    }
}

struct DeviceFlow { id: String, token: String, url: String, expires: f64, interval: f64 }
fn parse_device_flow(payload: &Value, token: String) -> Result<DeviceFlow, ExtensionFailure> {
    if payload.get("code").and_then(Value::as_i64) != Some(0) { return Err(failure("GLM ZCode cli init response was not a successful payload")); }
    let body = payload.get("data").filter(|body| body.is_object());
    let url = body.and_then(|body| body.get("authorize_url")).and_then(Value::as_str).filter(|url| reqwest::Url::parse(url).is_ok_and(|url| url.scheme() == "https")).ok_or_else(|| failure("GLM ZCode cli init response missing a valid https data.authorize_url"))?;
    let id = body.and_then(|body| body.get("flow_id")).and_then(Value::as_str).filter(|id| !id.is_empty()).ok_or_else(|| failure("GLM ZCode cli init response missing data.flow_id"))?;
    let expires = body.and_then(|body| body.get("expires_at")).and_then(Value::as_f64).filter(|value| value.is_finite()).ok_or_else(|| failure("GLM ZCode cli init response missing numeric data.expires_at"))?;
    let interval = body.and_then(|body| body.get("poll_interval_sec")).and_then(Value::as_f64).filter(|value| value.is_finite() && *value >= 1.0).ok_or_else(|| failure("GLM ZCode cli init response missing data.poll_interval_sec >= 1"))?;
    let token = body.and_then(|body| body.get("poll_token")).and_then(Value::as_str).filter(|value| !value.trim().is_empty()).map(str::to_owned).unwrap_or(token);
    Ok(DeviceFlow { id: id.into(), token, url: url.into(), expires, interval })
}
fn login_tokens(payload: &Value) -> Option<(String, Option<String>)> {
    let body = data(payload).ok()?;
    let token = body.get("zai").and_then(|zai| zai.get("access_token")).and_then(Value::as_str).filter(|value| !value.is_empty()).or_else(|| body.get("access_token").and_then(Value::as_str).filter(|value| !value.is_empty()))?;
    Some((token.into(), body.get("token").and_then(Value::as_str).filter(|value| !value.is_empty()).map(str::to_owned)))
}
impl OAuthClient {
    async fn init_flow(&self, signal: &AbortSignal) -> Result<DeviceFlow, ExtensionFailure> {
        let token = rand::random::<[u8; 32]>().iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        let payload = self.post(&self.init, json!({"provider":"zai"}), signal, "cli init", Some(&token)).await?;
        parse_device_flow(&payload, token)
    }
    async fn poll_once(&self, flow: &DeviceFlow, signal: &AbortSignal) -> Result<Option<(String, Option<String>)>, ExtensionFailure> {
        let mut url = reqwest::Url::parse(&format!("{}/", self.poll)).map_err(|error| failure(error.to_string()))?;
        url.path_segments_mut().map_err(|_| failure("invalid CLI poll URL"))?.pop_if_empty().push(&flow.id);
        let operation = async {
            let response = match self.client.get(url).header("Accept", "application/json").bearer_auth(&flow.token).timeout(Duration::from_millis(REQUEST_TIMEOUT_MS)).send().await { Ok(response) => response, Err(_) => return Ok(None) };
            let status = response.status().as_u16();
            if status == 408 || status == 429 || status >= 500 { return Ok(None); }
            if status >= 400 { return Err(failure(format!("GLM ZCode cli poll request failed: {status}"))); }
            if !response.status().is_success() { return Ok(None); }
            Ok(response.json::<Value>().await.ok().as_ref().and_then(login_tokens))
        };
        tokio::select! { biased; _ = signal.cancelled() => Err(failure("GLM ZCode cli poll request cancelled")), result = operation => result }
    }
    async fn manual(&self, callbacks: &dyn OAuthLoginCallbacks, signal: &AbortSignal) -> Result<OAuthCredentials, ExtensionFailure> {
        let state = uuid::Uuid::new_v4().to_string();
        let mut url = reqwest::Url::parse(AUTHORIZE_URL).map_err(|error| failure(error.to_string()))?;
        url.query_pairs_mut().extend_pairs([("redirect_uri", REDIRECT_URI), ("response_type", "code"), ("client_id", "client_P8X5CMWmlaRO9gyO-KSqtg"), ("state", &state)]);
        callbacks.on_auth(OAuthAuthInfo { url: url.into(), instructions: Some("Complete Z.AI login in your browser. This is an unofficial ZCode-based login without PKCE support; keep the final zcode:// redirect URL private, then paste it here.".into()) });
        let input = match callbacks.on_manual_code_input() { Some(input) => input.await, None => callbacks.on_prompt(OAuthPrompt { message: "Paste the ZCode redirect URL".into(), ..Default::default() }).await };
        let payload = self.post(&self.broker, json!({"provider":"zai","code":callback_code(&input, &state)?,"redirect_uri":REDIRECT_URI,"state":state}), signal, "broker", None).await?;
        let body = data(&payload)?;
        let upstream = body.get("zai").and_then(|zai| zai.get("access_token")).and_then(Value::as_str).filter(|value| !value.is_empty()).ok_or_else(|| failure("GLM ZCode broker response missing data.zai.access_token"))?;
        callbacks.on_progress("Provisioning Z.AI API key...");
        let mut credential = self.provision(upstream, signal).await?;
        if let Some(jwt) = body.get("token").and_then(Value::as_str).filter(|value| !value.is_empty()) { credential.extra.insert("zcodeJwtToken".into(), json!(jwt)); }
        Ok(credential)
    }
    pub async fn login(&self, callbacks: &dyn OAuthLoginCallbacks) -> Result<OAuthCredentials, ExtensionFailure> {
        let signal = maho_ai::utils::abort::operation_signal(callbacks.signal());
        let flow = match self.init_flow(&signal).await { Ok(flow) => flow, Err(error) if signal.aborted() => return Err(error), Err(_) => return self.manual(callbacks, &signal).await };
        callbacks.on_auth(OAuthAuthInfo { url: flow.url.clone(), instructions: Some("Complete the Z.AI login in your browser. This is an unofficial ZCode-based device flow and may break at any time; this session picks up the authorization code automatically, so there is nothing to paste.".into()) });
        callbacks.on_progress("Waiting for Z.AI login to complete...");
        let (upstream, jwt) = loop {
            if now_ms() / 1000.0 >= flow.expires { return Err(failure("GLM ZCode login flow expired before completion")); }
            if let Some(tokens) = self.poll_once(&flow, &signal).await? { break tokens; }
            tokio::select! { biased; _ = signal.cancelled() => return Err(failure("GLM ZCode cli poll request cancelled")), _ = tokio::time::sleep(Duration::from_secs_f64(flow.interval)) => {} }
        };
        callbacks.on_progress("Provisioning Z.AI API key...");
        let mut credential = self.provision(&upstream, &signal).await?;
        if let Some(jwt) = jwt { credential.extra.insert("zcodeJwtToken".into(), json!(jwt)); }
        Ok(credential)
    }
    pub async fn refresh(&self, credential: &OAuthCredentials, signal: &AbortSignal) -> Result<OAuthCredentials, ExtensionFailure> {
        if credential.refresh.is_empty() { return Err(failure("GLM ZCode credentials require re-login (`/login glm-zcode`); no stored upstream Z.AI token")); }
        let mut refreshed = self.provision(&credential.refresh, signal).await.map_err(|error| failure(format!("GLM ZCode credentials require re-login (`/login glm-zcode`); re-provisioning the Z.AI API key failed ({})", redact_secrets(&format!("Error: {error}")))))?;
        if let Some(jwt) = credential.get_extra_str("zcodeJwtToken").filter(|value| !value.is_empty()) { refreshed.extra.insert("zcodeJwtToken".into(), json!(jwt)); }
        Ok(refreshed)
    }
}
impl ExtensionOAuthConfig for OAuthClient {
    fn name(&self) -> &str { "GLM ZCode (unofficial)" }
    fn login<'a>(&'a self, callbacks: &'a dyn OAuthLoginCallbacks) -> ExtensionFuture<'a, OAuthCredentials> { Box::pin(self.login(callbacks)) }
    fn refresh_token<'a>(&'a self, credential: &'a OAuthCredentials, signal: &'a AbortSignal) -> ExtensionFuture<'a, OAuthCredentials> { Box::pin(self.refresh(credential, signal)) }
    fn get_api_key(&self, credential: &OAuthCredentials) -> String { credential.access.clone() }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn expired_init_never_polls_or_prompts() {
        use std::io::{BufRead,Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1",0)).unwrap(); let init = format!("http://{}/init",listener.local_addr().unwrap());
        let peer = std::thread::spawn(move || { let (mut stream,_) = listener.accept().unwrap(); stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap(); let mut reader = std::io::BufReader::new(stream.try_clone().unwrap()); let mut line = String::new(); loop { line.clear(); reader.read_line(&mut line).unwrap(); if line == "\r\n" { break; } } let body = json!({"code":0,"data":{"flow_id":"expired","authorize_url":"https://example.test","expires_at":0,"poll_interval_sec":1}}).to_string(); write!(stream,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap(); });
        let callbacks = Callbacks { auth: Default::default() }; let error = OAuthClient { init, ..Default::default() }.login(&callbacks).await.unwrap_err(); assert!(error.to_string().contains("expired before completion")); peer.join().unwrap();
    }
    #[tokio::test]
    async fn poll_fatal_status_and_retry_statuses_match_device_protocol() {
        use std::io::{BufRead,Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1",0)).unwrap(); let poll = format!("http://{}/poll",listener.local_addr().unwrap());
        let peer = std::thread::spawn(move || { for status in [403,408,429,500] { let (mut stream,_) = listener.accept().unwrap(); stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap(); let mut reader = std::io::BufReader::new(stream.try_clone().unwrap()); let mut line = String::new(); reader.read_line(&mut line).unwrap(); assert!(line.starts_with("GET /poll/fixture ")); loop { line.clear(); reader.read_line(&mut line).unwrap(); if line == "\r\n" { break; } } write!(stream,"HTTP/1.1 {status} fixture\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}} ").unwrap(); } });
        let client = OAuthClient { poll, ..Default::default() }; let flow = DeviceFlow { id: "fixture".into(),token: "token".into(),url: "https://example.test".into(),expires: 0.0,interval: 1.0 }; let signal = maho_ai::utils::abort::operation_signal(None);
        assert!(client.poll_once(&flow,&signal).await.unwrap_err().to_string().contains("cli poll request failed: 403")); for _ in 0..3 { assert!(client.poll_once(&flow,&signal).await.unwrap().is_none()); } peer.join().unwrap();
    }
    struct Callbacks { auth: std::sync::Mutex<Option<String>> }
    impl OAuthLoginCallbacks for Callbacks {
        fn on_auth(&self, info: OAuthAuthInfo) { *self.auth.lock().expect("capture authorization") = Some(info.url); }
        fn on_device_code(&self, _: maho_ai::oauth::OAuthDeviceCodeInfo) {}
        fn on_prompt(&self, _: OAuthPrompt) -> maho_ai::types::BoxFuture<'_,String> { Box::pin(async { panic!("unexpected paste prompt") }) }
        fn on_select(&self, _: maho_ai::oauth::OAuthSelectPrompt) -> maho_ai::types::BoxFuture<'_,Option<String>> { Box::pin(async { None }) }
    }
    async fn login_fixture(manual: bool, server_token: bool, jwt: bool) {
        use std::io::{BufRead,Read,Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1",0)).unwrap();
        let base = format!("http://{}",listener.local_addr().unwrap());
        let peer = std::thread::spawn(move || {
            let init_payload = if manual { json!({"code":0,"data":{}}) } else { json!({"code":0,"data":{"flow_id":"fixture","poll_token":if server_token { "poll-token" } else { "" },"authorize_url":"https://example.test/authorize","expires_at":now_ms()/1000.0+120.0,"poll_interval_sec":1}}) };
            let mut login_payload = json!({"data":{"zai":{"access_token":"upstream"}}}); if jwt { login_payload["data"]["token"] = json!("jwt"); }
            let mut init_auth = String::new();
            for (method,path,payload) in [
                ("POST","/init",init_payload),
                (if manual { "POST" } else { "GET" },if manual { "/broker" } else { "/poll/fixture" },login_payload),
                ("POST","/api/auth/z/login",json!({"data":{"access_token":"business"}})),
                ("GET","/api/biz/customer/getCustomerInfo",json!({"data":{"organizations":[{"organizationId":"org","projects":[{"projectId":"project"}]}]}})),
                ("GET","/api/biz/v1/organization/org/projects/project/api_keys",json!({"data":[]})),
                ("POST","/api/biz/v1/organization/org/projects/project/api_keys",json!({"data":{"apiKey":"key"}})),
                ("GET","/api/biz/v1/organization/org/projects/project/api_keys/copy/key",json!({"data":{"secretKey":"secret"}})),
            ] {
                let (mut stream,_) = listener.accept().unwrap(); stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap()); let mut line = String::new(); reader.read_line(&mut line).unwrap(); assert!(line.starts_with(&format!("{method} {path} ")),"{line}");
                let mut headers = String::new(); let mut length = 0;
                loop { line.clear(); reader.read_line(&mut line).unwrap(); if line == "\r\n" { break; } let lower = line.to_lowercase(); if let Some(value) = lower.strip_prefix("content-length:") { length = value.trim().parse().unwrap(); } headers.push_str(&lower); }
                let mut body = vec![0;length]; reader.read_exact(&mut body).unwrap();
                if path == "/init" { assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(),json!({"provider":"zai"})); init_auth = headers.lines().find(|line|line.starts_with("authorization:")).unwrap().into(); }
                if path == "/poll/fixture" { assert!(headers.contains(if server_token { "authorization: bearer poll-token" } else { &init_auth })); }
                if path == "/broker" { let payload: Value = serde_json::from_slice(&body).unwrap(); assert_eq!(payload["code"],"fixture-code"); assert_eq!(payload["redirect_uri"],"zcode://oauth/callback"); assert!(!payload["state"].as_str().unwrap().is_empty()); }
                if method == "POST" && path.ends_with("api_keys") { assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(),json!({"name":"zcode-api-key"})); }
                let body = payload.to_string(); write!(stream,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
        });
        let client = OAuthClient { init: format!("{base}/init"), poll: format!("{base}/poll"), broker: format!("{base}/broker"), api: base, ..Default::default() };
        let callbacks = Callbacks { auth: Default::default() };
        let credentials = client.login(&FixtureCallbacks { inner: &callbacks, manual }).await.unwrap(); peer.join().unwrap();
        assert_eq!(credentials.access,"key.secret"); assert_eq!(credentials.refresh,"upstream"); assert_eq!(credentials.get_extra_str("zcodeJwtToken"),jwt.then_some("jwt"));
        if !manual { assert_eq!(callbacks.auth.lock().unwrap().as_deref(),Some("https://example.test/authorize")); }
    }
    struct FixtureCallbacks<'a> { inner: &'a Callbacks, manual: bool }
    impl OAuthLoginCallbacks for FixtureCallbacks<'_> {
        fn on_auth(&self, info: OAuthAuthInfo) { self.inner.on_auth(info); }
        fn on_device_code(&self, _: maho_ai::oauth::OAuthDeviceCodeInfo) {}
        fn on_select(&self, _: maho_ai::oauth::OAuthSelectPrompt) -> maho_ai::types::BoxFuture<'_,Option<String>> { Box::pin(async { None }) }
        fn on_prompt(&self, prompt: OAuthPrompt) -> maho_ai::types::BoxFuture<'_,String> { self.inner.on_prompt(prompt) }
        fn on_manual_code_input(&self) -> Option<maho_ai::types::BoxFuture<'_,String>> { self.manual.then(|| Box::pin(async { let url = self.inner.auth.lock().unwrap().clone().unwrap(); let state = reqwest::Url::parse(&url).unwrap().query_pairs().find(|(key,_)|key == "state").unwrap().1.into_owned(); format!("zcode://oauth/callback?code=fixture-code&state={state}") }) as maho_ai::types::BoxFuture<'_,String>) }
    }
    #[tokio::test] async fn device_login_persists_jwt_without_manual_prompt() { login_fixture(false,true,true).await; }
    #[tokio::test] async fn missing_server_token_uses_client_bearer_and_omits_absent_jwt() { login_fixture(false,false,false).await; }
    #[tokio::test] async fn malformed_init_falls_back_to_manual_broker_and_preserves_jwt() { login_fixture(true,false,true).await; }
    #[test] fn callback_preserves_percent_encoded_code() { assert_eq!(callback_code("zcode://oauth/callback?code=a%2Bb&state=s", "s").unwrap(), "a+b"); }
    #[test] fn callback_rejects_duplicate_fields() { assert!(callback_code("zcode://oauth/callback?code=a&code=b&state=s", "s").is_err()); }
    #[test] fn callback_rejects_wrong_state() { assert!(callback_code("zcode://oauth/callback?code=a&state=wrong", "s").is_err()); }
    #[test] fn callback_rejects_bare_code() { assert!(callback_code("auth-code","s").is_err()); }
    #[test] fn callback_rejects_port() { assert!(callback_code("zcode://oauth:1234/callback?code=a&state=s","s").is_err()); }
    #[test] fn callback_requires_state() { assert!(callback_code("zcode://oauth/callback?code=a","s").is_err()); }
    #[test] fn callback_rejects_wrong_origin() { for url in ["https://oauth/callback?code=a&state=s", "zcode://other/callback?code=a&state=s", "zcode://oauth/callback?code=a&state=s#fragment"] { assert!(callback_code(url, "s").is_err()); } }
    #[test] fn flow_requires_valid_https_url() { let payload = json!({"code":0,"data":{"authorize_url":"http://invalid","flow_id":"id","expires_at":123,"poll_interval_sec":1}}); assert!(parse_device_flow(&payload, "fallback".into()).is_err()); }
    #[test] fn flow_uses_client_token_when_server_token_blank() { let payload = json!({"code":0,"data":{"authorize_url":"https://example.test","flow_id":"id","expires_at":123,"poll_interval_sec":1,"poll_token":" "}}); assert_eq!(parse_device_flow(&payload, "fallback".into()).unwrap().token, "fallback"); }
    #[test] fn flow_requires_positive_poll_interval() { let payload = json!({"code":0,"data":{"authorize_url":"https://example.test","flow_id":"id","expires_at":123,"poll_interval_sec":0}}); assert!(parse_device_flow(&payload, "fallback".into()).is_err()); }
    #[test] fn flow_requires_id() { let payload = json!({"code":0,"data":{"authorize_url":"https://example.test","expires_at":123,"poll_interval_sec":1}}); assert!(parse_device_flow(&payload,"fallback".into()).is_err()); }
    #[test] fn flow_requires_success_code() { let payload = json!({"code":1,"data":{"authorize_url":"https://example.test","flow_id":"id","expires_at":123,"poll_interval_sec":1}}); assert!(parse_device_flow(&payload,"fallback".into()).is_err()); }
    #[test] fn tokens_prefer_nested_zai_token() { assert_eq!(login_tokens(&json!({"data":{"zai":{"access_token":"nested"},"access_token":"other","token":"jwt"}})), Some(("nested".into(), Some("jwt".into())))); }
    #[test] fn tokens_fall_back_to_flat_token() { assert_eq!(login_tokens(&json!({"access_token":"flat"})), Some(("flat".into(), None))); }
    #[test] fn tokens_wait_when_absent() { assert!(login_tokens(&json!({"data":{"state":"pending"}})).is_none()); }
    #[test] fn secrets_are_redacted() { assert_eq!(redact_secrets(&"a".repeat(40)), "[redacted]"); assert_eq!(redact_secrets("eyJabc.def.ghi"), "[redacted-jwt]"); }
    #[tokio::test] async fn cancelled_request_does_not_contact_endpoint() { let controller = maho_ai::utils::abort::AbortController::new(); controller.abort(None); let client = OAuthClient::default(); assert!(client.provision("fixture", &controller.signal()).await.unwrap_err().to_string().contains("cancelled")); }
    #[tokio::test] async fn refresh_requires_stored_upstream_token() { let credential = OAuthCredentials::new("key","",0.0); assert!(OAuthClient::default().refresh(&credential,&maho_ai::utils::abort::operation_signal(None)).await.unwrap_err().to_string().contains("no stored upstream")); }
    #[tokio::test] async fn refresh_passes_cancellation_into_provisioning() { let controller = maho_ai::utils::abort::AbortController::new(); controller.abort(None); let credential = OAuthCredentials::new("key","upstream",0.0); assert!(OAuthClient::default().refresh(&credential,&controller.signal()).await.unwrap_err().to_string().contains("cancelled")); }
    #[tokio::test]
    async fn provisioning_uses_default_project_and_reuses_named_key() {
        use std::io::{BufRead, Read, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1",0)).unwrap();
        let base = format!("http://{}",listener.local_addr().unwrap());
        let peer = std::thread::spawn(move || {
            let replies = [
                ("POST /api/auth/z/login ",json!({"data":{"access_token":"business"}})),
                ("GET /api/biz/customer/getCustomerInfo ",json!({"data":{"email":"User@Example.com","id":42,"organizations":[{"organizationId":"wrong","projects":[]},{"organizationId":"org","isDefault":true,"projects":[{"projectId":"wrong"},{"projectId":"project","isDefault":true}]}]}})),
                ("GET /api/biz/v1/organization/org/projects/project/api_keys ",json!({"data":[{"name":"other","apiKey":"other"},{"name":"zcode-api-key","apiKey":"key-id"}]})),
                ("GET /api/biz/v1/organization/org/projects/project/api_keys/copy/key-id ",json!({"data":{"secretKey":"fixture-secret"}})),
            ];
            for (expected,payload) in replies {
                let (mut stream,_) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new(); reader.read_line(&mut line).unwrap(); assert!(line.starts_with(expected),"{line}");
                let mut length = 0; let mut authorization = String::new();
                loop { line.clear(); reader.read_line(&mut line).unwrap(); if line == "\r\n" { break; } let lower = line.to_lowercase(); if let Some(value) = lower.strip_prefix("content-length:") { length = value.trim().parse().unwrap(); } else if let Some(value) = lower.strip_prefix("authorization:") { authorization = value.trim().into(); } }
                if expected.starts_with("GET") { assert_eq!(authorization,"bearer business"); }
                if length > 0 { let mut body = vec![0;length]; reader.read_exact(&mut body).unwrap(); assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(),json!({"token":"upstream"})); }
                let body = payload.to_string(); write!(stream,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
        });
        let client = OAuthClient { api: base, ..Default::default() };
        let credential = client.provision("upstream",&maho_ai::utils::abort::operation_signal(None)).await.unwrap();
        peer.join().unwrap();
        assert_eq!(credential.access,"key-id.fixture-secret");
        assert_eq!(credential.refresh,"upstream");
        assert_eq!(credential.get_extra_str("email"),Some("user@example.com"));
        assert_eq!(credential.get_extra_str("accountId"),Some("42"));
        assert!(credential.expires > now_ms() + 315_000_000_000.0);
    }
}
