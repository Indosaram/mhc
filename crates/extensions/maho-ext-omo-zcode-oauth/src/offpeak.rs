use maho_ext_api::ProviderModelConfig;
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Duration};
pub const OFFPEAK_BASE_URL: &str = "https://zcode.z.ai/api/v1/off-peak/anthropic";
const TICKET_BASE_URL: &str = "https://zcode.z.ai/api/v1/off-peak/ticket";
pub const SIGNATURE_HEADERS: [&str; 7] = ["X-Client-Ts","X-Client-Version","X-Client-Sig","X-Client-Nonce","X-Client-Pow","X-App-Id","X-Client-Sign-Verified"];
pub fn is_flash_model_id(id: &str) -> bool { id.to_lowercase().contains("flash") }
pub fn is_routed(base_url: Option<&str>) -> bool { base_url == Some(OFFPEAK_BASE_URL) }
pub fn is_window(now: f64) -> bool { ((now / 3_600_000.0).floor() + 9.0).rem_euclid(24.0) < 10.0 }
pub fn window_epoch(now: f64) -> f64 { ((now - 32_400_000.0) / 86_400_000.0).floor() }
#[derive(Clone)]
pub struct Ticket { pub api_key: String, pub jwt: String, pub id: String, pub taken_at: f64, pub epoch: f64 }
impl Ticket {
    pub fn new(api_key: &str, jwt: &str, id: String, now: f64) -> Self { Self { api_key: api_key.into(), jwt: jwt.into(), id, taken_at: now, epoch: window_epoch(now) } }
    pub fn fresh(&self, now: f64, api_key: &str, jwt: &str) -> bool { self.api_key == api_key && self.jwt == jwt && (self.epoch - window_epoch(now)).abs() < f64::EPSILON && now - self.taken_at < 600_000.0 && is_window(now) }
}
pub fn request_headers(jwt: &str, key: &str, id: &str) -> BTreeMap<String, String> { [("Authorization",format!("Bearer {jwt}")),("X-Coding-Plan-Api-Key",key.into()),("X-Off-Peak-Ticket-ID",id.into())].into_iter().map(|(key,value)|(key.into(),value)).collect() }
pub async fn install_auth<F: std::future::Future<Output=Option<String>>>(headers: &mut BTreeMap<String,Option<String>>, now: f64, credential: Option<(&str,&str)>, ensure: impl FnOnce(String,String) -> F) -> bool {
    if headers.get("X-ZCode-Route").and_then(|value|value.as_deref()) != Some("off-peak") { return false; }
    headers.remove("X-ZCode-Route");
    if !is_window(now) { return false; }
    let authorization = headers.get("Authorization").and_then(|value|value.as_deref()).unwrap_or_default().trim();
    let mut words = authorization.split_whitespace();
    if !words.next().is_some_and(|value|value.eq_ignore_ascii_case("Bearer")) { return false; }
    let Some(key) = words.next().filter(|_|words.next().is_none()) else { return false; };
    let Some((cached,jwt)) = credential.filter(|(cached,_)|*cached == key) else { return false; };
    let Some(ticket) = ensure(jwt.into(),cached.into()).await.filter(|ticket|!ticket.is_empty()) else { return false; };
    for (key,value) in request_headers(jwt,cached,&ticket) { headers.insert(key,Some(value)); }
    true
}
pub fn route(models: Vec<ProviderModelConfig>, active: bool) -> Vec<ProviderModelConfig> {
    let override_url = std::env::var("ZCODE_ANTHROPIC_BASE_URL").ok().map(|value| value.trim().to_owned()).filter(|value| !value.is_empty());
    route_with(models, active, override_url, crate::models::resolve_base_url())
}
pub fn route_with(models: Vec<ProviderModelConfig>, active: bool, override_url: Option<String>, ultra: String) -> Vec<ProviderModelConfig> {
    let fallback = override_url.clone().unwrap_or(ultra);
    models.into_iter().map(|mut model| {
        if active && override_url.is_none() && is_flash_model_id(&model.id) { model.base_url = Some(OFFPEAK_BASE_URL.into()); }
        else {
            model.base_url = Some(override_url.clone().unwrap_or_else(|| model.base_url.clone().filter(|url| !url.is_empty() && url != OFFPEAK_BASE_URL).unwrap_or_else(|| fallback.clone())));
            if let Some(headers) = &mut model.headers && headers.remove("X-ZCode-Route").is_some() && headers.is_empty() { model.headers = None; }
        }
        model
    }).collect()
}
fn poll_interval(value: Option<&Value>) -> f64 { value.and_then(Value::as_f64).filter(|value| value.is_finite()).map_or(10_000.0, |value| value * 1000.0).clamp(2000.0,30_000.0) }
pub struct TicketClient { pub client: reqwest::Client, pub base: String, pub headers: BTreeMap<String, String>, pub now: std::sync::Arc<dyn Fn() -> f64 + Send + Sync> }
impl TicketClient {
    pub fn new(headers: BTreeMap<String, String>) -> Self { Self { client: reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap_or_default(), base: TICKET_BASE_URL.into(), headers, now: std::sync::Arc::new(crate::oauth::now_ms) } }
    async fn fetch(&self, path: &str, jwt: &str, key: &str, body: Option<Value>) -> Option<Value> {
        let url = format!("{}{path}",self.base);
        let mut request = if let Some(body) = body { self.client.post(url).json(&body) } else { self.client.get(url) };
        for (key,value) in &self.headers { request = request.header(key,value); }
        let response = request.bearer_auth(jwt).header("X-Coding-Plan-Api-Key",key).timeout(Duration::from_secs(15)).send().await.ok()?;
        if !response.status().is_success() { return None; }
        response.json().await.ok()
    }
    pub async fn availability(&self, jwt: &str, key: &str) -> bool { self.fetch("/availability",jwt,key,None).await.is_some_and(|payload| payload.get("data").filter(|value| value.is_object()).unwrap_or(&payload).get("can_take_number") == Some(&Value::Bool(true))) }
    pub async fn ensure(&self, jwt: &str, key: &str, task: &str) -> Option<String> {
        self.ensure_with(jwt,key,task,|| (self.now)(),|ms|tokio::time::sleep(Duration::from_secs_f64(ms/1000.0))).await
    }
    async fn ensure_with<F: std::future::Future<Output=()>>(&self, jwt: &str, key: &str, task: &str, now: impl Fn() -> f64, wait: impl Fn(f64) -> F) -> Option<String> {
        let payload = self.fetch("",jwt,key,Some(json!({"task_id":task}))).await?;
        let ticket = payload.get("data").filter(|value| value.is_object()).unwrap_or(&payload);
        let id = ticket.get("ticket_id").and_then(Value::as_str).filter(|value| !value.is_empty())?.to_owned();
        let mut state = ticket.get("state").and_then(Value::as_str).unwrap_or_default().to_owned();
        let mut interval = poll_interval(ticket.get("next_poll_after"));
        let deadline = now() + 120_000.0;
        loop {
            if state == "ready" || state == "active" { return Some(id); }
            if state != "queued" || now() + interval > deadline { return None; }
            wait(interval).await;
            let payload = self.fetch("/status",jwt,key,Some(json!({"ticket_ids":[id]}))).await?;
            let body = payload.get("data").filter(|value| value.is_object()).unwrap_or(&payload);
            let ticket = body.get("tickets").and_then(Value::as_array)?.iter().find(|ticket| ticket.get("ticket_id").and_then(Value::as_str) == Some(&id))?;
            state = ticket.get("state").and_then(Value::as_str).unwrap_or("queued").into();
            if let Some(value) = ticket.get("next_poll_after") { interval = poll_interval(Some(value)); }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn final_permitted_ticket_poll_returns_ready_or_stops_when_queued() {
        use std::io::{BufRead,Write};
        for ready in [false,true] {
            let listener = std::net::TcpListener::bind(("127.0.0.1",0)).unwrap(); let base = format!("http://{}",listener.local_addr().unwrap());
            let peer = std::thread::spawn(move || { for step in 0..=4 { let (mut stream,_) = listener.accept().unwrap(); stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap(); let mut reader = std::io::BufReader::new(stream.try_clone().unwrap()); let mut line = String::new(); reader.read_line(&mut line).unwrap(); assert!(line.starts_with(if step == 0 { "POST / " } else { "POST /status " })); loop { line.clear(); reader.read_line(&mut line).unwrap(); if line == "\r\n" { break; } } let ticket = json!({"ticket_id":"fixture","state":if ready && step == 4 { "ready" } else { "queued" },"next_poll_after":30}); let payload = if step == 0 { ticket } else { json!({"tickets":[ticket]}) }; let body = payload.to_string(); write!(stream,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap(); } });
            let client = TicketClient { base, ..TicketClient::new(BTreeMap::new()) }; let clock = std::cell::Cell::new(0.0);
            let result = client.ensure_with("jwt","key","task",||clock.get(),|ms| { assert!((ms-30_000.0).abs() < f64::EPSILON); clock.set(clock.get()+ms); std::future::ready(()) }).await;
            assert_eq!(result.as_deref(),ready.then_some("fixture")); assert!((clock.get()-120_000.0).abs() < f64::EPSILON); peer.join().unwrap();
        }
    }
    #[test] fn window_boundaries_use_kst() { assert!(is_window(15.0*3_600_000.0)); assert!(is_window(0.0)); assert!(!is_window(3_600_000.0)); }
    #[test] fn poll_interval_is_clamped() { assert!((poll_interval(Some(&json!(0))) - 2000.0).abs() < f64::EPSILON); assert!((poll_interval(Some(&json!(100))) - 30_000.0).abs() < f64::EPSILON); }
    #[test] fn ticket_is_bound_to_credentials() { let ticket = Ticket::new("key","jwt","ticket".into(),0.0); assert!(ticket.fresh(100.0,"key","jwt")); assert!(!ticket.fresh(100.0,"other","jwt")); }
    #[test] fn ticket_expires_after_ten_minutes() { let ticket = Ticket::new("key","jwt","ticket".into(),0.0); assert!(!ticket.fresh(600_000.0,"key","jwt")); }
    #[test] fn request_headers_carry_all_ticket_auth() { let headers = request_headers("jwt","key","ticket"); assert_eq!(headers["Authorization"],"Bearer jwt"); assert_eq!(headers["X-Off-Peak-Ticket-ID"],"ticket"); }
    #[test] fn inactive_route_keeps_existing_base() { let mut model = crate::models::model_config("m".into(),"M".into()); model.base_url = Some("https://custom.test".into()); assert_eq!(route(vec![model],false)[0].base_url.as_deref(),Some("https://custom.test")); }
    #[tokio::test]
    async fn real_ticket_protocol_sends_auth_and_task_identity() {
        use std::io::{BufRead,Read,Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1",0)).unwrap(); let base = format!("http://{}/ticket",listener.local_addr().unwrap());
        let peer = std::thread::spawn(move || {
            for path in ["/ticket/availability","/ticket"] {
                let (mut stream,_) = listener.accept().unwrap(); stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap()); let mut line = String::new(); reader.read_line(&mut line).unwrap(); assert!(line.contains(path));
                let mut headers = String::new(); let mut length = 0;
                loop { line.clear(); reader.read_line(&mut line).unwrap(); if line == "\r\n" { break; } let lower = line.to_lowercase(); if let Some(value) = lower.strip_prefix("content-length:") { length = value.trim().parse().unwrap(); } headers.push_str(&lower); }
                assert!(headers.contains("authorization: bearer jwt")); assert!(headers.contains("x-coding-plan-api-key: key"));
                let payload = if path.ends_with("availability") { json!({"data":{"can_take_number":true}}) } else {
                    let mut body = vec![0;length]; reader.read_exact(&mut body).unwrap(); assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(),json!({"task_id":"fixture-task"})); json!({"ticket_id":"fixture-ticket","state":"ready"})
                };
                let body = payload.to_string(); write!(stream,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
        });
        let client = TicketClient { base, ..TicketClient::new(BTreeMap::new()) };
        assert!(client.availability("jwt","key").await); assert_eq!(client.ensure("jwt","key","fixture-task").await.as_deref(),Some("fixture-ticket")); peer.join().unwrap();
    }
    #[test] fn ticket_cannot_cross_window_epoch() { let ticket = Ticket::new("key","jwt","ticket".into(),0.0); assert!(!ticket.fresh(86_400_000.0,"key","jwt")); }
    #[test] fn signature_header_inventory_matches_upstream() { assert_eq!(SIGNATURE_HEADERS.len(),7); assert!(SIGNATURE_HEADERS.contains(&"X-Client-Sig")); assert!(!SIGNATURE_HEADERS.contains(&"X-Session-Id")); }
    #[test]
    fn flash_family_and_route_marker_detection_matches_upstream() {
        assert!(is_flash_model_id("glm-5.3-flash")); assert!(is_flash_model_id("GLM-5.3-Flash")); assert!(!is_flash_model_id("glm-5.3"));
        assert!(is_routed(Some(OFFPEAK_BASE_URL))); assert!(!is_routed(Some("https://other.test"))); assert!(!is_routed(None));
    }
    #[test]
    fn routing_strips_the_marker_and_preserves_other_headers() {
        let mut model = crate::models::model_config("glm-5.3".into(),"GLM".into()); model.base_url = Some("https://ultra.test".into());
        model.headers = Some(BTreeMap::from([("X-ZCode-Route".into(),"off-peak".into()),("X-Title".into(),"fixture".into())]));
        let routed = route_with(vec![model.clone()],false,None,"https://ultra.test".into());
        assert_eq!(routed[0].base_url.as_deref(),Some("https://ultra.test")); assert_eq!(routed[0].headers.as_ref().and_then(|headers|headers.get("X-Title")).map(String::as_str),Some("fixture"));
        assert!(!routed[0].headers.as_ref().is_some_and(|headers|headers.contains_key("X-ZCode-Route")));
        model.headers = Some(BTreeMap::from([("X-ZCode-Route".into(),"off-peak".into())]));
        assert!(route_with(vec![model],false,None,"https://ultra.test".into())[0].headers.is_none());
    }
    #[test]
    fn routing_keeps_markerless_headers_and_override_beats_offpeak() {
        let mut model = crate::models::model_config("glm-5.3-flash".into(),"Flash".into()); model.headers = Some(BTreeMap::from([("X-Title".into(),"fixture".into())]));
        let active = route_with(vec![model.clone()],true,None,"https://ultra.test".into());
        assert_eq!(active[0].base_url.as_deref(),Some(OFFPEAK_BASE_URL)); assert_eq!(active[0].headers.as_ref().and_then(|headers|headers.get("X-Title")).map(String::as_str),Some("fixture"));
        let overridden = route_with(vec![model],true,Some("https://override.test".into()),"https://ultra.test".into());
        assert_eq!(overridden[0].base_url.as_deref(),Some("https://override.test"));
    }
    #[tokio::test]
    async fn marker_auth_installs_only_for_matching_credential_in_window() {
        let mut headers = BTreeMap::from([("X-ZCode-Route".into(),Some("off-peak".into())),("Authorization".into(),Some("Bearer key".into()))]);
        assert!(install_auth(&mut headers,0.0,Some(("key","jwt")),|jwt,key| async move { assert_eq!((jwt.as_str(),key.as_str()),("jwt","key")); Some("ticket".into()) }).await);
        assert!(!headers.contains_key("X-ZCode-Route")); assert_eq!(headers["Authorization"].as_deref(),Some("Bearer jwt")); assert_eq!(headers["X-Off-Peak-Ticket-ID"].as_deref(),Some("ticket"));
    }
    #[tokio::test]
    async fn marker_auth_fails_open_outside_window_or_on_credential_mismatch() {
        for (now,key) in [(3_600_000.0,"key"),(0.0,"other")] {
            let mut headers = BTreeMap::from([("X-ZCode-Route".into(),Some("off-peak".into())),("Authorization".into(),Some("Bearer key".into()))]);
            assert!(!install_auth(&mut headers,now,Some((key,"jwt")),|_,_|async { panic!("must not acquire ticket") }).await);
            assert_eq!(headers["Authorization"].as_deref(),Some("Bearer key")); assert!(!headers.contains_key("X-ZCode-Route"));
        }
    }
    #[tokio::test]
    async fn unmarked_headers_are_untouched_and_failed_ticket_keeps_api_key() {
        let mut headers = BTreeMap::from([("Authorization".into(),Some("Bearer key".into()))]); let original = headers.clone();
        assert!(!install_auth(&mut headers,0.0,Some(("key","jwt")),|_,_|async { panic!("unmarked request must not acquire") }).await); assert_eq!(headers,original);
        headers.insert("X-ZCode-Route".into(),Some("off-peak".into())); assert!(!install_auth(&mut headers,0.0,Some(("key","jwt")),|_,_|async { None }).await); assert_eq!(headers,original);
        headers.insert("X-ZCode-Route".into(),Some("off-peak".into())); assert!(!install_auth(&mut headers,0.0,Some(("key","jwt")),|_,_|async { Some(String::new()) }).await); assert_eq!(headers,original);
    }
    #[tokio::test]
    async fn availability_reads_false_for_refusal_http_failure_and_bad_shape() {
        use std::io::{BufRead,Write};
        for (status,payload) in [(200,json!({"data":{"can_take_number":false}})),(500,json!({})),(200,json!({"data":{"can_take_number":"yes"}}))] {
            let listener = std::net::TcpListener::bind(("127.0.0.1",0)).unwrap(); let base = format!("http://{}",listener.local_addr().unwrap());
            let peer = std::thread::spawn(move || {
                let (mut stream,_) = listener.accept().unwrap(); stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap()); let mut line = String::new(); reader.read_line(&mut line).unwrap(); assert!(line.starts_with("GET /availability "),"{line}");
                loop { line.clear(); reader.read_line(&mut line).unwrap(); if line == "\r\n" { break; } }
                let body = payload.to_string(); write!(stream,"HTTP/1.1 {status} fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            });
            let client = TicketClient { base, ..TicketClient::new(BTreeMap::new()) };
            assert!(!client.availability("jwt","key").await); peer.join().unwrap();
        }
        let client = TicketClient { base: "http://127.0.0.1:1".into(), ..TicketClient::new(BTreeMap::new()) };
        assert!(!client.availability("jwt","key").await);
    }
    #[tokio::test]
    async fn queued_ticket_gives_up_when_the_status_poll_fails() {
        use std::io::{BufRead,Read,Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1",0)).unwrap(); let base = format!("http://{}",listener.local_addr().unwrap());
        let peer = std::thread::spawn(move || {
            for step in 0..=1 {
                let (mut stream,_) = listener.accept().unwrap(); stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap()); let mut line = String::new(); reader.read_line(&mut line).unwrap(); assert!(line.starts_with(if step == 0 { "POST / " } else { "POST /status " }),"{line}");
                let mut length = 0; loop { line.clear(); reader.read_line(&mut line).unwrap(); if line == "\r\n" { break; } else if let Some(value) = line.to_lowercase().strip_prefix("content-length:") { length = value.trim().parse().unwrap(); } }
                let mut body = vec![0;length]; reader.read_exact(&mut body).unwrap();
                if step == 0 { let payload = json!({"ticket_id":"fixture","state":"queued","next_poll_after":1}).to_string(); write!(stream,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",payload.len()).unwrap(); }
                else { write!(stream,"HTTP/1.1 503 fixture\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap(); }
            }
        });
        let client = TicketClient { base, ..TicketClient::new(BTreeMap::new()) }; let clock = std::cell::Cell::new(0.0);
        let result = client.ensure_with("jwt","key","task",||clock.get(),|ms| { clock.set(clock.get()+ms); std::future::ready(()) }).await;
        assert!(result.is_none()); assert!((clock.get()-2_000.0).abs() < f64::EPSILON); peer.join().unwrap();
    }
}
