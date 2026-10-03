use maho_ext_api::ProviderModelConfig;
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Duration};
pub const OFFPEAK_BASE_URL: &str = "https://zcode.z.ai/api/v1/off-peak/anthropic";
const TICKET_BASE_URL: &str = "https://zcode.z.ai/api/v1/off-peak/ticket";
pub const SIGNATURE_HEADERS: [&str; 7] = ["X-Client-Ts","X-Client-Version","X-Client-Sig","X-Client-Nonce","X-Client-Pow","X-App-Id","X-Client-Sign-Verified"];
pub fn is_window(now: f64) -> bool { ((now / 3_600_000.0).floor() + 9.0).rem_euclid(24.0) < 10.0 }
pub fn window_epoch(now: f64) -> f64 { ((now - 32_400_000.0) / 86_400_000.0).floor() }
#[derive(Clone)]
pub struct Ticket { pub api_key: String, pub jwt: String, pub id: String, pub taken_at: f64, pub epoch: f64 }
impl Ticket {
    pub fn new(api_key: &str, jwt: &str, id: String, now: f64) -> Self { Self { api_key: api_key.into(), jwt: jwt.into(), id, taken_at: now, epoch: window_epoch(now) } }
    pub fn fresh(&self, now: f64, api_key: &str, jwt: &str) -> bool { self.api_key == api_key && self.jwt == jwt && (self.epoch - window_epoch(now)).abs() < f64::EPSILON && now - self.taken_at < 600_000.0 && is_window(now) }
}
pub fn request_headers(jwt: &str, key: &str, id: &str) -> BTreeMap<String, String> { [("Authorization",format!("Bearer {jwt}")),("X-Coding-Plan-Api-Key",key.into()),("X-Off-Peak-Ticket-ID",id.into())].into_iter().map(|(key,value)|(key.into(),value)).collect() }
pub fn route(models: Vec<ProviderModelConfig>, active: bool) -> Vec<ProviderModelConfig> {
    let override_url = std::env::var("ZCODE_ANTHROPIC_BASE_URL").ok().map(|value| value.trim().to_owned()).filter(|value| !value.is_empty());
    let fallback = override_url.clone().unwrap_or_else(crate::models::resolve_base_url);
    models.into_iter().map(|mut model| {
        if active && override_url.is_none() && model.id.to_lowercase().contains("flash") { model.base_url = Some(OFFPEAK_BASE_URL.into()); }
        else {
            model.base_url = Some(override_url.clone().unwrap_or_else(|| model.base_url.clone().filter(|url| !url.is_empty() && url != OFFPEAK_BASE_URL).unwrap_or_else(|| fallback.clone())));
            if let Some(headers) = &mut model.headers { headers.remove("X-ZCode-Route"); if headers.is_empty() { model.headers = None; } }
        }
        model
    }).collect()
}
fn poll_interval(value: Option<&Value>) -> f64 { value.and_then(Value::as_f64).filter(|value| value.is_finite()).map_or(10_000.0, |value| value * 1000.0).clamp(2000.0,30_000.0) }
pub struct TicketClient { pub client: reqwest::Client, pub base: String, pub headers: BTreeMap<String, String> }
impl TicketClient {
    pub fn new(headers: BTreeMap<String, String>) -> Self { Self { client: reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap_or_default(), base: TICKET_BASE_URL.into(), headers } }
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
        self.ensure_with(jwt,key,task,crate::oauth::now_ms,|ms|tokio::time::sleep(Duration::from_secs_f64(ms/1000.0))).await
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
}
