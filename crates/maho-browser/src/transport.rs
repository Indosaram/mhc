//! Transports: Chromium launched with `--remote-debugging-pipe` (NUL-delimited JSON on fds 3/4),
//! Chromium launched with `--remote-debugging-port=0` (WebSocket; the endpoint is read from the
//! browser's "DevTools listening on" line), and an existing DevTools endpoint (`http://host:port`
//! or `ws://...`).
//!
//! Port of omowright `src/pipe.js` (`PipeCdpClient.ensureConnected`, `#awaitReady`, `close`,
//! the bounded stdio tail) and the endpoint discovery of the WebSocket `CdpClient` in `src/core.js`
//! (`/json/version` → `webSocketDebuggerUrl`). The pipe file descriptors are wired by `/bin/sh`
//! (`3<&0 4>&1`) because this workspace forbids the `unsafe` a `pre_exec` `dup2` would need.
use crate::cdp::{CdpClient, Reader, Writer};
use serde_json::{Value, json};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, ChildStderr, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// omowright `DEFAULT_READINESS_TIMEOUT_MS`.
pub const READINESS_TIMEOUT: Duration = Duration::from_secs(10);
/// omowright `STDIO_TAIL_LIMIT`.
const STDIO_TAIL_LIMIT: usize = 16_384;
/// omowright `GRACEFUL_CLOSE_TIMEOUT_MS`.
pub const GRACEFUL_CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

/// The last bytes the browser wrote to stdout/stderr, for launch diagnostics.
#[derive(Clone, Default)]
pub struct StdioTail(Arc<Mutex<String>>);

impl StdioTail {
    fn push(&self, chunk: &str) {
        let mut tail = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        tail.push_str(chunk);
        if tail.len() > STDIO_TAIL_LIMIT {
            let mut cut = tail.len() - STDIO_TAIL_LIMIT;
            while !tail.is_char_boundary(cut) {
                cut += 1;
            }
            tail.drain(..cut);
        }
    }

    pub fn text(&self) -> String {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .trim()
            .to_owned()
    }
}

fn drain_into(stream: impl Read + Send + 'static, tail: StdioTail) {
    let _ = std::thread::Builder::new()
        .name("browser-stdio".into())
        .spawn(move || {
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|read| read > 0) {
                tail.push(&line);
                line.clear();
            }
        });
}

/// A launched browser process and its CDP connection.
pub struct Launched {
    pub client: CdpClient,
    pub child: Child,
    pub tail: StdioTail,
}

/// Polls `Browser.getVersion` until the browser answers (omowright `#awaitReady`).
fn await_ready(client: &CdpClient, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    let mut last_error = String::from("no response");
    while Instant::now() < deadline {
        match client.send_with_timeout(
            "Browser.getVersion",
            json!({}),
            None,
            Duration::from_secs(1),
        ) {
            Ok(_) => return Ok(()),
            Err(error) => last_error = error,
        }
        if !client.is_connected() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(last_error)
}

/// Stops a browser that failed to come up and describes why, with what it printed.
fn fail_launch(child: &mut Child, tail: &StdioTail, reason: &str) -> String {
    // Let a browser that is exiting on its own finish, so its last words reach the tail.
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline && matches!(child.try_wait(), Ok(None)) {
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    let mut text = tail.text();
    let settle = Instant::now() + Duration::from_millis(300);
    while text.is_empty() && Instant::now() < settle {
        std::thread::sleep(Duration::from_millis(20));
        text = tail.text();
    }
    format!(
        "Browser did not become ready over CDP ({reason}){}",
        if text.is_empty() {
            String::new()
        } else {
            format!("\nBrowser stdio tail:\n{text}")
        }
    )
}

/// Launches `executable` with `args` and `--remote-debugging-pipe` and connects over the pipe.
#[cfg(unix)]
pub fn launch_pipe(
    executable: &Path,
    args: &[String],
    readiness: Duration,
) -> Result<Launched, String> {
    // fd 3 = our writes (the child's stdin pipe), fd 4 = our reads (the child's stdout pipe); the
    // browser's own stdin is /dev/null and its stdout joins stderr, which is drained into the tail
    // so startup noise never reaches the CDP stream or the caller's terminal.
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg("exec \"$0\" \"$@\" 3<&0 4>&1 0</dev/null 1>&2")
        .arg(executable)
        .args(args)
        .arg("--remote-debugging-pipe")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot launch {}: {error}", executable.display()))?;
    let tail = StdioTail::default();
    if let Some(stderr) = child.stderr.take() {
        drain_into(stderr, tail.clone());
    }
    let (Some(mut input), Some(output)) = (child.stdin.take(), child.stdout.take()) else {
        let _ = child.kill();
        return Err("browser pipes are unavailable".into());
    };
    let writer: Writer = Box::new(move |message| {
        input.write_all(message.as_bytes())?;
        input.write_all(b"\0")?;
        input.flush()
    });
    let mut output = BufReader::new(output);
    let reader: Reader = Box::new(move || {
        let mut buffer = Vec::new();
        loop {
            buffer.clear();
            if output.read_until(0, &mut buffer)? == 0 {
                return Ok(None);
            }
            if buffer.last() == Some(&0) {
                buffer.pop();
            }
            if !buffer.is_empty() {
                return Ok(Some(String::from_utf8_lossy(&buffer).into_owned()));
            }
        }
    });
    let client = CdpClient::new(writer, reader);
    if let Err(error) = await_ready(&client, readiness) {
        return Err(fail_launch(&mut child, &tail, &error));
    }
    let _ = client.send("Target.setDiscoverTargets", json!({"discover": true}), None);
    Ok(Launched {
        client,
        child,
        tail,
    })
}

/// Reads stderr until Chromium prints its DevTools endpoint, then keeps draining into `tail`.
fn devtools_endpoint(
    stderr: ChildStderr,
    tail: &StdioTail,
    timeout: Duration,
) -> Result<String, String> {
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let background = tail.clone();
    std::thread::Builder::new()
        .name("browser-stderr".into())
        .spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            let mut sent = false;
            while reader.read_line(&mut line).is_ok_and(|read| read > 0) {
                background.push(&line);
                if !sent && let Some(rest) = line.trim().strip_prefix("DevTools listening on ") {
                    let _ = tx.send(rest.trim().to_owned());
                    sent = true;
                }
                line.clear();
            }
        })
        .map_err(|error| format!("cannot read the browser's stderr: {error}"))?;
    rx.recv_timeout(timeout).map_err(|_| {
        format!(
            "the browser did not print its DevTools endpoint within {}s\nBrowser stdio tail:\n{}",
            timeout.as_secs(),
            tail.text()
        )
    })
}

/// Launches `executable` with `--remote-debugging-port=0` and connects to the WebSocket endpoint it
/// prints. Used where the pipe transport is unavailable (Windows) and on request elsewhere.
pub fn launch_port(
    executable: &Path,
    args: &[String],
    readiness: Duration,
) -> Result<Launched, String> {
    let mut child = Command::new(executable)
        .args(args)
        .arg("--remote-debugging-port=0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot launch {}: {error}", executable.display()))?;
    let tail = StdioTail::default();
    if let Some(stdout) = child.stdout.take() {
        drain_into(stdout, tail.clone());
    }
    let Some(stderr) = child.stderr.take() else {
        let _ = child.kill();
        return Err("browser stderr is unavailable".into());
    };
    let connected = devtools_endpoint(stderr, &tail, readiness)
        .and_then(|endpoint| connect(&endpoint))
        .and_then(|client| await_ready(&client, readiness).map(|()| client));
    match connected {
        Ok(client) => {
            let _ = client.send("Target.setDiscoverTargets", json!({"discover": true}), None);
            Ok(Launched {
                client,
                child,
                tail,
            })
        }
        Err(error) => Err(fail_launch(&mut child, &tail, &error)),
    }
}

/// `host:port` and path of an `http://` or `ws://` URL (no TLS; DevTools endpoints are local).
fn split_url(url: &str) -> Result<(String, String), String> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("ws://"))
        .ok_or_else(|| {
            format!("unsupported DevTools endpoint {url:?}: expected http://host:port or ws://...")
        })?;
    let (authority, path) = rest
        .find('/')
        .map_or((rest, "/"), |index| (&rest[..index], &rest[index..]));
    if authority.is_empty() {
        return Err(format!("DevTools endpoint {url:?} has no host"));
    }
    let authority = if authority.contains(':') {
        authority.to_owned()
    } else {
        format!("{authority}:80")
    };
    Ok((authority, path.to_owned()))
}

/// `GET /json/version` → `webSocketDebuggerUrl` (omowright `discoverVersion`).
pub fn discover_websocket_url(http_url: &str) -> Result<String, String> {
    let (authority, _) = split_url(http_url)?;
    let mut stream = TcpStream::connect(&authority)
        .map_err(|error| format!("cannot reach the DevTools endpoint {authority}: {error}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| error.to_string())?;
    write!(
        stream,
        "GET /json/version HTTP/1.1\r\nHost: {authority}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    )
    .map_err(|error| error.to_string())?;
    // Read until the declared body length (or EOF): a peer that closes right after answering may
    // reset the connection, which must not discard a complete response.
    let mut raw = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        if let Some(end) = raw.windows(4).position(|window| window == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&raw[..end]).to_ascii_lowercase();
            let length = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok());
            if length.is_some_and(|length| raw.len() >= end + 4 + length) {
                break;
            }
        }
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => raw.extend_from_slice(&chunk[..read]),
            Err(error) if raw.is_empty() => {
                return Err(format!("DevTools discovery failed: {error}"));
            }
            Err(_) => break,
        }
    }
    let response = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = response
        .split_once("\r\n\r\n")
        .ok_or("DevTools discovery returned no HTTP body")?;
    if !head
        .lines()
        .next()
        .is_some_and(|status| status.contains(" 200"))
    {
        return Err(format!(
            "CDP discovery request failed ({})",
            head.lines().next().unwrap_or_default()
        ));
    }
    let json_start = body
        .find('{')
        .ok_or("DevTools discovery body is not JSON")?;
    let json_end = body.rfind('}').map_or(body.len(), |end| end + 1);
    let version: Value = serde_json::from_str(&body[json_start..json_end])
        .map_err(|error| format!("DevTools discovery body is not JSON: {error}"))?;
    version
        .get("webSocketDebuggerUrl")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "DevTools discovery has no webSocketDebuggerUrl".into())
}

fn is_timeout(error: &tungstenite::Error) -> bool {
    matches!(error, tungstenite::Error::Io(io) if matches!(io.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut))
}

/// Connects to an existing DevTools endpoint (`http://host:port` or a `ws://` browser URL).
pub fn connect(endpoint: &str) -> Result<CdpClient, String> {
    let endpoint = endpoint.trim().trim_end_matches('/');
    let ws_url = if endpoint.starts_with("ws://") {
        endpoint.to_owned()
    } else {
        discover_websocket_url(endpoint)?
    };
    let (authority, _) = split_url(&ws_url)?;
    let stream = TcpStream::connect(&authority)
        .map_err(|error| format!("cannot reach {authority}: {error}"))?;
    let (socket, _) = tungstenite::client::client(ws_url.as_str(), stream)
        .map_err(|error| format!("CDP WebSocket handshake with {ws_url} failed: {error}"))?;
    // A short read timeout lets the reader release the socket so writers can send.
    socket
        .get_ref()
        .set_read_timeout(Some(Duration::from_millis(20)))
        .map_err(|error| error.to_string())?;
    let socket = Arc::new(Mutex::new(socket));
    let write_socket = Arc::clone(&socket);
    let writer: Writer = Box::new(move |message| {
        let mut socket = write_socket.lock().unwrap_or_else(PoisonError::into_inner);
        socket
            .send(tungstenite::Message::text(message))
            .map_err(io::Error::other)
    });
    let reader: Reader = Box::new(move || {
        loop {
            let read = socket.lock().unwrap_or_else(PoisonError::into_inner).read();
            match read {
                Ok(tungstenite::Message::Text(text)) => return Ok(Some(text.as_str().to_owned())),
                Ok(tungstenite::Message::Binary(bytes)) => {
                    return Ok(Some(String::from_utf8_lossy(&bytes).into_owned()));
                }
                Ok(tungstenite::Message::Close(_)) => return Ok(None),
                Ok(_) => {}
                Err(error) if is_timeout(&error) => std::thread::yield_now(),
                Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                    return Ok(None);
                }
                Err(error) => return Err(io::Error::other(error)),
            }
        }
    });
    Ok(CdpClient::new(writer, reader))
}

/// Closes a launched browser: `Browser.close`, a grace period, then a kill (omowright `close`).
pub fn shutdown(client: &CdpClient, child: &mut Child) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    if client.is_connected() {
        let _ = client.send_with_timeout("Browser.close", json!({}), None, GRACEFUL_CLOSE_TIMEOUT);
    }
    let deadline = Instant::now() + GRACEFUL_CLOSE_TIMEOUT;
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_split_into_authority_and_path() {
        assert_eq!(
            split_url("http://127.0.0.1:9222").ok(),
            Some(("127.0.0.1:9222".into(), "/".into()))
        );
        assert_eq!(
            split_url("ws://localhost:9222/devtools/browser/abc").ok(),
            Some(("localhost:9222".into(), "/devtools/browser/abc".into()))
        );
        assert_eq!(
            split_url("ws://example/x").ok(),
            Some(("example:80".into(), "/x".into()))
        );
        assert!(split_url("https://127.0.0.1:9222").is_err());
    }

    #[test]
    fn stdio_tail_keeps_the_last_bytes() {
        let tail = StdioTail::default();
        tail.push(&"a".repeat(STDIO_TAIL_LIMIT));
        tail.push("end\n");
        let text = tail.text();
        assert_eq!(text.len(), STDIO_TAIL_LIMIT - 1);
        assert!(text.ends_with("end"));
    }

    #[test]
    fn discovery_reads_the_websocket_url_from_json_version() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0");
        let Ok(listener) = listener else { return };
        let port = listener
            .local_addr()
            .map(|addr| addr.port())
            .unwrap_or_default();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut request = Vec::new();
                let mut byte = [0u8; 1];
                while !request.ends_with(b"\r\n\r\n")
                    && stream.read(&mut byte).is_ok_and(|n| n == 1)
                {
                    request.push(byte[0]);
                }
                let body = r#"{"Browser":"Fake/1","webSocketDebuggerUrl":"ws://127.0.0.1:1/devtools/browser/x"}"#;
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        assert_eq!(
            discover_websocket_url(&format!("http://127.0.0.1:{port}")).ok(),
            Some("ws://127.0.0.1:1/devtools/browser/x".into())
        );
    }
}
