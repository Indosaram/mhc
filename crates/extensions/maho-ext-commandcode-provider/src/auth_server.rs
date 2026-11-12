use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::sync::oneshot;

const DEFAULT_PORT: u16 = 51789;
const PORT_RANGE: u16 = 20;
const MAX_BODY_BYTES: usize = 10_000;
const ALLOWED_ORIGINS: [&str; 3] = [
    "http://localhost:3000",
    "https://staging.commandcode.ai",
    "https://commandcode.ai",
];

type CallbackSender = Arc<Mutex<Option<oneshot::Sender<Result<String, String>>>>>;

pub struct CallbackServer {
    port: u16,
    state: String,
    receiver: oneshot::Receiver<Result<String, String>>,
}

impl CallbackServer {
    pub async fn start() -> std::io::Result<Self> {
        Self::start_in_range(DEFAULT_PORT, PORT_RANGE).await
    }

    pub async fn start_in_range(start_port: u16, range: u16) -> std::io::Result<Self> {
        let state = state_token();
        let listener = bind_available(start_port, range).await?;
        let port = listener.local_addr()?.port();
        let (sender, receiver) = oneshot::channel();
        let sender = Arc::new(Mutex::new(Some(sender)));
        let expected = state.clone();
        tokio::spawn(async move {
            if let Err(error) = serve(listener, expected, sender).await {
                let _ = error;
            }
        });
        Ok(Self {
            port,
            state,
            receiver,
        })
    }

    pub fn auth_url(&self) -> String {
        let callback = url::form_urlencoded::byte_serialize(
            format!("http://localhost:{}/callback", self.port).as_bytes(),
        )
        .collect::<String>();
        let state = url::form_urlencoded::byte_serialize(self.state.as_bytes()).collect::<String>();
        format!(
            "{}/studio/auth/cli?callback={callback}&state={state}",
            crate::oauth::STUDIO_BASE_URL
        )
    }

    pub async fn wait(self, timeout: std::time::Duration) -> Result<String, String> {
        match tokio::time::timeout(timeout, self.receiver).await {
            Err(_) => Err("Browser authentication timed out".to_owned()),
            Ok(Err(_)) => Err("Command Code callback server stopped".to_owned()),
            Ok(Ok(result)) => result,
        }
    }
}

async fn bind_available(start_port: u16, range: u16) -> std::io::Result<TcpListener> {
    let mut last = None;
    for offset in 0..range {
        let port = start_port + offset;
        match TcpListener::bind(("127.0.0.1", port)).await {
            Ok(listener) => return Ok(listener),
            Err(error) => last = Some(error),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("no free callback port")))
}

fn state_token() -> String {
    let mut bytes = [0u8; 32];
    if getrandom_fill(&mut bytes).is_err() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        for (index, slot) in bytes.iter_mut().enumerate() {
            *slot = (now >> (index % 16 * 8)) as u8;
        }
    }
    url_safe_base64(&bytes)
}

fn url_safe_base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let triple = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(ALPHABET[(triple >> 18) as usize & 63] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(triple >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[triple as usize & 63] as char);
        }
    }
    out
}

fn getrandom_fill(bytes: &mut [u8]) -> Result<(), ()> {
    use std::io::Read;
    let mut source = std::fs::File::open("/dev/urandom").map_err(|_| ())?;
    source.read_exact(bytes).map_err(|_| ())
}

async fn serve(
    listener: TcpListener,
    expected: String,
    sender: CallbackSender,
) -> std::io::Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let outcome = handle(stream, &expected, &sender).await;
        match outcome {
            Outcome::Completed => return Ok(()),
            Outcome::Retry => continue,
        }
    }
}

enum Outcome {
    Completed,
    Retry,
}

async fn handle(mut stream: TcpStream, expected: &str, sender: &CallbackSender) -> Outcome {
    let Some(request) = read_request(&mut stream).await else {
        return Outcome::Retry;
    };
    let origin = request
        .headers
        .get("origin")
        .filter(|origin| ALLOWED_ORIGINS.contains(&origin.as_str()))
        .cloned()
        .unwrap_or_else(|| ALLOWED_ORIGINS[0].to_owned());
    let requested_headers = request
        .headers
        .get("access-control-request-headers")
        .cloned()
        .unwrap_or_else(|| "Content-Type".to_owned());
    let cors = vec![
        ("Access-Control-Allow-Origin".to_owned(), origin),
        (
            "Access-Control-Allow-Methods".to_owned(),
            "POST, OPTIONS".to_owned(),
        ),
        ("Access-Control-Allow-Headers".to_owned(), requested_headers),
        (
            "Access-Control-Allow-Private-Network".to_owned(),
            "true".to_owned(),
        ),
    ];

    if request.method == "OPTIONS" {
        respond(&mut stream, 204, "", &cors).await;
        return Outcome::Retry;
    }
    if request.path != "/callback" {
        respond_json(&mut stream, 404, "Not found", &cors).await;
        return Outcome::Retry;
    }
    if request.method != "POST" {
        respond_json(&mut stream, 405, "Method not allowed. Use POST.", &cors).await;
        return Outcome::Retry;
    }
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&request.body) else {
        respond_json(&mut stream, 400, "Invalid JSON", &cors).await;
        return Outcome::Retry;
    };
    if let Some(error) = parsed.get("error").and_then(|value| value.as_str()) {
        let description = parsed
            .get("error_description")
            .and_then(|value| value.as_str())
            .unwrap_or(error);
        let message = if error == "access_denied" {
            if description.is_empty() {
                "Authorization was denied by the user".to_owned()
            } else {
                description.to_owned()
            }
        } else if description.is_empty() {
            error.to_owned()
        } else {
            description.to_owned()
        };
        respond_json(&mut stream, 200, "", &cors).await;
        if let Some(sender) = sender.lock().await.take() {
            let _ = sender.send(Err(message));
        }
        return Outcome::Completed;
    }
    let field = |name: &str| {
        parsed
            .get(name)
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_owned()
    };
    let (api_key, state, user_id, user_name, key_name) = (
        field("apiKey"),
        field("state"),
        field("userId"),
        field("userName"),
        field("keyName"),
    );
    if api_key.is_empty()
        || state.is_empty()
        || user_id.is_empty()
        || user_name.is_empty()
        || key_name.is_empty()
    {
        respond_json(&mut stream, 400, "Missing required fields", &cors).await;
        return Outcome::Retry;
    }
    if state != expected {
        respond_json(&mut stream, 403, "Invalid state token", &cors).await;
        return Outcome::Retry;
    }
    respond_json(&mut stream, 200, "", &cors).await;
    if let Some(sender) = sender.lock().await.take() {
        let _ = sender.send(Ok(api_key));
    }
    Outcome::Completed
}

struct Request {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: String,
}

async fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut raw = Vec::new();
    let mut buffer = [0u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut buffer).await.ok()?;
        if read == 0 {
            return None;
        }
        raw.extend_from_slice(&buffer[..read]);
        if let Some(position) = find(&raw, b"\r\n\r\n") {
            break position;
        }
        if raw.len() > 64 * 1024 {
            return None;
        }
    };
    let head = String::from_utf8_lossy(&raw[..header_end]).to_string();
    let mut lines = head.lines();
    let mut request_line = lines.next()?.split_whitespace();
    let method = request_line.next()?.to_owned();
    let path = request_line.next()?.to_owned();
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_lowercase(), value.trim().to_owned());
        }
    }
    let length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0)
        .min(MAX_BODY_BYTES);
    let mut body = raw[header_end + 4..].to_vec();
    while body.len() < length {
        let read = stream.read(&mut buffer).await.ok()?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&buffer[..read]);
    }
    Some(Request {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).to_string(),
    })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

async fn respond_json(stream: &mut TcpStream, status: u16, error: &str, cors: &[(String, String)]) {
    let body = if error.is_empty() {
        "{\"success\":true}".to_owned()
    } else {
        serde_json::json!({ "success": false, "error": error }).to_string()
    };
    respond(stream, status, &body, cors).await;
}

async fn respond(stream: &mut TcpStream, status: u16, body: &str, cors: &[(String, String)]) {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "OK",
    };
    let mut response = format!("HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n");
    for (name, value) in cors {
        response.push_str(&format!("{name}: {value}\r\n"));
    }
    response.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ));
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.flush().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn post(port: u16, path: &str, body: &str) -> String {
        let stream = TcpStream::connect(("127.0.0.1", port));
        let mut stream = tokio::time::timeout(std::time::Duration::from_secs(5), stream)
            .await
            .expect("connect within timeout")
            .expect("connect");
        let request = format!(
            "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nOrigin: https://commandcode.ai\r\n\r\n{body}",
            body.len()
        );
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            stream.write_all(request.as_bytes()),
        )
        .await
        .expect("write within timeout")
        .expect("write");
        let mut response = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            stream.read_to_string(&mut response),
        )
        .await
        .expect("read within timeout")
        .expect("read");
        response
    }

    fn body(port: u16) -> String {
        format!(
            "{{\"apiKey\":\"cc_live_key\",\"state\":\"{port}\",\"userId\":\"u\",\"userName\":\"n\",\"keyName\":\"k\"}}"
        )
    }

    #[tokio::test]
    async fn a_matching_post_delivers_the_key() {
        let server = CallbackServer::start_in_range(52310, 1)
            .await
            .expect("start");
        let port = server.port;
        let state = server.state.clone();
        let payload = body(port).replace(&port.to_string(), &state);
        let waiter =
            tokio::spawn(async move { server.wait(std::time::Duration::from_secs(5)).await });
        let response = post(port, "/callback", &payload).await;
        assert!(response.starts_with("HTTP/1.1 200"));
        assert_eq!(waiter.await.expect("join").expect("key"), "cc_live_key");
    }

    #[tokio::test]
    async fn a_wrong_state_is_rejected_and_keeps_waiting() {
        let server = CallbackServer::start_in_range(52320, 1)
            .await
            .expect("start");
        let port = server.port;
        let waiter =
            tokio::spawn(async move { server.wait(std::time::Duration::from_secs(1)).await });
        let response = post(port, "/callback", &body(port)).await;
        assert!(response.starts_with("HTTP/1.1 403"));
        assert!(waiter.await.expect("join").is_err());
    }

    #[tokio::test]
    async fn a_denied_login_fails_with_the_description() {
        let server = CallbackServer::start_in_range(52330, 1)
            .await
            .expect("start");
        let port = server.port;
        let waiter =
            tokio::spawn(async move { server.wait(std::time::Duration::from_secs(5)).await });
        let response = post(
            port,
            "/callback",
            "{\"error\":\"access_denied\",\"error_description\":\"nope\"}",
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 200"));
        assert_eq!(waiter.await.expect("join").expect_err("denied"), "nope");
    }
}
