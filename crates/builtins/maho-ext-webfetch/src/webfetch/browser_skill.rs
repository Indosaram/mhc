use maho_tools::definition::AbortSignal;
use serde::Serialize;
use serde_json::{Value, json};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Semaphore;
use tokio::time::Instant;

/// Reads a page through the browser the user already has connected to BrowserSkill (Tencent's `bsk`
/// daemon plus its extension), the integration OmOWright's `connectBrowserSkill` drives. webfetch never
/// launches a browser and never starts the daemon: when the integration is not connected and in use,
/// the caller keeps its static result and reports a typed skip (senpi#5939, senpi#5940; design in
/// senpi#5940 issuecomment-6651347167).
///
/// Two session modes: `incognito` renders a page the user did not list in an incognito Agent Window,
/// so none of the profile's cookies are sent; `signed-in` renders a listed host in a normal window
/// with the user's session. Wire shapes follow `crates/bsk-protocol` (JSON Lines over the daemon's
/// Unix socket or Windows named pipe).

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedPage {
    pub url: String,
    pub html: String,
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserMode {
    Incognito,
    SignedIn,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectedBrowser {
    pub instance_id: String,
    pub browser_name: String,
    pub label: String,
}

#[derive(Clone, Debug)]
pub struct PageRenderOptions {
    pub mode: BrowserMode,
    pub timeout_ms: u64,
    pub signal: Option<AbortSignal>,
    pub browser: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedPageOutcome {
    pub page: RenderedPage,
    pub browser: ConnectedBrowser,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WebfetchBrowserSkipReason {
    Disabled,
    NoTimeLeft,
    IntegrationNotConnected,
    IncognitoUnsupported,
    IncognitoUnavailable,
    Timeout,
    RenderFailed,
    NoMoreText,
    HostNotAllowed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WebfetchNotConnectedCause {
    NoDaemon,
    DaemonUnreachable,
    Incompatible,
    NoBrowser,
    BrowserUnresponsive,
    PinnedBrowserMissing,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WebfetchBrowserSkip {
    pub reason: WebfetchBrowserSkipReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cause: Option<WebfetchNotConnectedCause>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

pub const NOT_CONNECTED_MESSAGE: &str = "browser integration not connected; render skipped";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenderError {
    Aborted,
    Skip(WebfetchBrowserSkip),
    Other(String),
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Aborted => f.write_str("The operation was aborted."),
            Self::Skip(skip) => match &skip.detail {
                Some(detail) => write!(f, "{} ({detail})", skip.message),
                None => f.write_str(&skip.message),
            },
            Self::Other(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for RenderError {}

pub type RenderFuture<'a> =
    Pin<Box<dyn Future<Output = Result<RenderedPageOutcome, RenderError>> + Send + 'a>>;

pub trait PageRenderer: Send + Sync {
    fn name(&self) -> &str;
    fn render<'a>(&'a self, url: &'a str, options: PageRenderOptions) -> RenderFuture<'a>;
}

fn not_connected(cause: WebfetchNotConnectedCause, detail: impl Into<String>) -> RenderError {
    RenderError::Skip(WebfetchBrowserSkip {
        reason: WebfetchBrowserSkipReason::IntegrationNotConnected,
        cause: Some(cause),
        message: NOT_CONNECTED_MESSAGE.into(),
        detail: Some(detail.into()),
    })
}

fn skip(
    reason: WebfetchBrowserSkipReason,
    message: impl Into<String>,
    detail: Option<String>,
) -> RenderError {
    RenderError::Skip(WebfetchBrowserSkip {
        reason,
        cause: None,
        message: message.into(),
        detail,
    })
}

#[derive(Clone, Debug, Default)]
pub struct BrowserSkillOptions {
    pub home: Option<PathBuf>,
    pub settle_poll_ms: Option<u64>,
    pub status_timeout_ms: Option<u64>,
    /// The uid that must own the daemon socket; defaults to this process's uid. Tests inject a mismatch.
    pub owner_uid: Option<u32>,
    pub max_sessions: Option<usize>,
    pub platform: Option<&'static str>,
}

const SETTLE_POLL_MS: u64 = 250;
// The page counts as rendered once two reads in a row return the same HTML length.
const SETTLED_READS: usize = 2;
const STATUS_TIMEOUT_MS: u64 = 2_000;
const STOP_TIMEOUT_MS: u64 = 5_000;
const CANCEL_TIMEOUT_MS: u64 = 2_000;
// navigate leaves this much of the budget for the HTML reads that follow it.
const GET_HTML_RESERVE_MS: u64 = 1_500;
const MAX_HTML_BYTES: usize = 5 * 1024 * 1024;
const MAX_SESSIONS: usize = 2;
// A page whose URL changes across this many reads in a row is still navigating and is refused.
const MAX_UNSTEADY_READS: usize = 8;
// A page whose HTML never settles (live counters, carousels) is read at most this many times; the last
// steady read is returned instead of spending the whole request timeout and losing it to a late failure.
const MAX_SETTLE_READS: usize = 12;
// How long a failed session.start is given to still answer, so a session the daemon created is stopped.
const LATE_SESSION_WAIT_MS: u64 = 1_500;
const WINDOWS_PIPE_PREFIX: &str = "\\\\.\\pipe\\";

fn browser_skill_home() -> PathBuf {
    if let Ok(override_dir) = std::env::var("BSK_HOME")
        && !override_dir.is_empty()
    {
        return PathBuf::from(override_dir);
    }
    PathBuf::from(maho_core::config::home_dir()).join(".bsk")
}

fn current_platform(override_platform: Option<&'static str>) -> &'static str {
    override_platform.unwrap_or(if cfg!(windows) { "win32" } else { "unix" })
}

/// The daemon's socket as named in its own daemon.json, refused unless it is a socket this user owns (or a named pipe).
fn resolve_daemon_socket(
    home: &Path,
    options: &BrowserSkillOptions,
) -> Result<String, RenderError> {
    let content = std::fs::read_to_string(home.join("daemon.json")).map_err(|_| {
        not_connected(
            WebfetchNotConnectedCause::NoDaemon,
            "no readable daemon.json",
        )
    })?;
    let info: Value = serde_json::from_str(&content).map_err(|_| {
        not_connected(
            WebfetchNotConnectedCause::NoDaemon,
            "no readable daemon.json",
        )
    })?;
    let sock_path = info
        .as_object()
        .and_then(|obj| obj.get("sock_path"))
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .ok_or_else(|| {
            not_connected(
                WebfetchNotConnectedCause::NoDaemon,
                "daemon.json names no socket",
            )
        })?
        .to_owned();

    if current_platform(options.platform) == "win32" {
        if !sock_path.starts_with(WINDOWS_PIPE_PREFIX) {
            return Err(not_connected(
                WebfetchNotConnectedCause::NoDaemon,
                "daemon.json socket is not a named pipe",
            ));
        }
        return Ok(sock_path);
    }

    if !Path::new(&sock_path).is_absolute() {
        return Err(not_connected(
            WebfetchNotConnectedCause::NoDaemon,
            "daemon.json socket path is not absolute",
        ));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let stat = std::fs::symlink_metadata(&sock_path).map_err(|_| {
            not_connected(
                WebfetchNotConnectedCause::DaemonUnreachable,
                "the daemon socket does not exist",
            )
        })?;
        if !stat.file_type().is_socket() {
            return Err(not_connected(
                WebfetchNotConnectedCause::NoDaemon,
                "daemon.json socket path is not a socket",
            ));
        }
        let expected_uid = options
            .owner_uid
            .unwrap_or_else(|| rustix::process::getuid().as_raw());
        if stat.uid() != expected_uid {
            return Err(not_connected(
                WebfetchNotConnectedCause::NoDaemon,
                "the daemon socket belongs to another user",
            ));
        }
    }

    Ok(sock_path)
}

#[derive(Clone, Debug)]
struct DaemonReply {
    id: Value,
    result: Option<Value>,
    error: Option<serde_json::Map<String, Value>>,
}

#[derive(Clone, Debug)]
enum DaemonError {
    Timeout(String),
    Unreachable(String),
    Aborted,
}

impl std::fmt::Display for DaemonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout(message) | Self::Unreachable(message) => f.write_str(message),
            Self::Aborted => f.write_str("The operation was aborted."),
        }
    }
}

fn random_hex_4() -> String {
    let mut bytes = [0u8; 4];
    let _ = getrandom::fill(&mut bytes);
    format!(
        "{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3]
    )
}

fn make_call_id(method: &str) -> String {
    let mut slug = String::with_capacity(method.len());
    let mut last_dash = false;
    for ch in method.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    format!("webfetch-{slug}-{}", random_hex_4())
}

#[cfg(unix)]
async fn call_daemon_unix(
    sock_path: &str,
    id: &str,
    method: &str,
    params: Value,
    timeout_ms: u64,
    signal: Option<&AbortSignal>,
) -> Result<DaemonReply, DaemonError> {
    let payload = format!(
        "{}\n",
        json!({ "id": id, "method": method, "params": params })
    );
    let io_future = async {
        let stream = tokio::net::UnixStream::connect(sock_path)
            .await
            .map_err(|error| DaemonError::Unreachable(error.to_string()))?;
        let (read_half, mut write_half) = stream.into_split();
        write_half
            .write_all(payload.as_bytes())
            .await
            .map_err(|error| DaemonError::Unreachable(error.to_string()))?;
        write_half
            .flush()
            .await
            .map_err(|error| DaemonError::Unreachable(error.to_string()))?;

        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        let read_bytes = reader
            .read_line(&mut line)
            .await
            .map_err(|error| DaemonError::Unreachable(error.to_string()))?;
        if read_bytes == 0 || !line.ends_with('\n') {
            return Err(DaemonError::Unreachable(format!(
                "{method}: connection closed without a reply"
            )));
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        let parsed: Value = serde_json::from_str(trimmed)
            .map_err(|_| DaemonError::Unreachable(format!("{method}: reply is not JSON")))?;
        let frame = parsed.as_object();
        let reply_id = frame
            .and_then(|obj| obj.get("id"))
            .cloned()
            .unwrap_or(Value::Null);
        let result = frame.and_then(|obj| obj.get("result")).cloned();
        let error = frame
            .and_then(|obj| obj.get("error"))
            .and_then(Value::as_object)
            .cloned();
        Ok(DaemonReply {
            id: reply_id,
            result,
            error,
        })
    };

    let duration = Duration::from_millis(timeout_ms.max(1));
    match signal {
        Some(signal) => {
            if signal.is_aborted() {
                return Err(DaemonError::Aborted);
            }
            tokio::select! {
                biased;
                () = signal.cancelled() => Err(DaemonError::Aborted),
                _ = tokio::time::sleep(duration) => Err(DaemonError::Timeout(format!(
                    "{method} did not answer within {timeout_ms}ms"
                ))),
                result = io_future => result,
            }
        }
        None => tokio::select! {
            _ = tokio::time::sleep(duration) => Err(DaemonError::Timeout(format!(
                "{method} did not answer within {timeout_ms}ms"
            ))),
            result = io_future => result,
        },
    }
}

#[cfg(not(unix))]
async fn call_daemon_unix(
    _sock_path: &str,
    _id: &str,
    _method: &str,
    _params: Value,
    _timeout_ms: u64,
    _signal: Option<&AbortSignal>,
) -> Result<DaemonReply, DaemonError> {
    Err(DaemonError::Unreachable(
        "daemon IPC is only supported on Unix sockets".into(),
    ))
}

async fn call_daemon(
    sock_path: &str,
    id: &str,
    method: &str,
    params: Value,
    timeout_ms: u64,
    signal: Option<&AbortSignal>,
) -> Result<DaemonReply, DaemonError> {
    call_daemon_unix(sock_path, id, method, params, timeout_ms, signal).await
}

fn protocol_parts(version: Option<&Value>) -> Option<(u64, u64)> {
    let text = version.and_then(Value::as_str)?;
    let mut parts = text.split('.');
    let major_str = parts.next()?;
    let minor_str = parts.next()?;
    let major: u64 = major_str
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()?;
    let minor_digits: String = minor_str.chars().take_while(char::is_ascii_digit).collect();
    if minor_digits.is_empty() {
        return None;
    }
    let minor: u64 = minor_digits.parse().ok()?;
    Some((major, minor))
}

fn supports_incognito(version: Option<&Value>) -> bool {
    matches!(protocol_parts(version), Some((1, minor)) if minor >= 4)
}

struct EligibleBrowser {
    browser: ConnectedBrowser,
    protocol: Option<Value>,
}

fn pick_browser(
    browsers: &[&serde_json::Map<String, Value>],
    preferred: Option<&str>,
) -> Result<EligibleBrowser, RenderError> {
    if browsers.is_empty() {
        return Err(not_connected(
            WebfetchNotConnectedCause::NoBrowser,
            "no browser is connected to the daemon",
        ));
    }
    let eligible: Vec<&serde_json::Map<String, Value>> = browsers
        .iter()
        .copied()
        .filter(|browser| {
            browser.get("unresponsive").and_then(Value::as_bool) != Some(true)
                && browser.get("instance_id").and_then(Value::as_str).is_some()
        })
        .collect();
    if eligible.is_empty() {
        return Err(not_connected(
            WebfetchNotConnectedCause::BrowserUnresponsive,
            "every connected browser stopped answering",
        ));
    }

    let pinned = preferred.and_then(|pref| {
        eligible.iter().copied().find(|browser| {
            browser.get("instance_id").and_then(Value::as_str) == Some(pref)
                || browser.get("label").and_then(Value::as_str) == Some(pref)
        })
    });
    // A pinned browser is the user's choice of signed-in profile; reading through another one would use a
    // session they did not pick, so a pin that matches nothing refuses the read instead of falling back.
    if let Some(pref) = preferred
        && pinned.is_none()
    {
        return Err(not_connected(
            WebfetchNotConnectedCause::PinnedBrowserMissing,
            format!("no connected browser matches the pinned browser \"{pref}\""),
        ));
    }

    let chosen = pinned.unwrap_or_else(|| {
        eligible.iter().copied().fold(eligible[0], |best, browser| {
            let best_ts = best
                .get("connected_at_ms")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            let cur_ts = browser
                .get("connected_at_ms")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            if cur_ts > best_ts { browser } else { best }
        })
    });

    let instance_id = chosen
        .get("instance_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let browser_name = chosen
        .get("browser_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let label = chosen
        .get("label")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let protocol = chosen.get("extension_protocol_version").cloned();

    Ok(EligibleBrowser {
        browser: ConnectedBrowser {
            instance_id,
            browser_name,
            label,
        },
        protocol,
    })
}

fn daemon_error(
    method: &str,
    mode: BrowserMode,
    error: &serde_json::Map<String, Value>,
) -> RenderError {
    let code = error.get("code").and_then(Value::as_str).unwrap_or("error");
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("daemon error");
    let detail = format!("{method}: {code}: {message}");
    let reason = error
        .get("data")
        .and_then(Value::as_object)
        .and_then(|data| data.get("reason"))
        .and_then(Value::as_str);

    if code == "no_browser_connected" || code == "not_found" {
        return not_connected(WebfetchNotConnectedCause::NoBrowser, detail);
    }
    if code == "timeout" && reason == Some("extension_unresponsive") {
        return not_connected(WebfetchNotConnectedCause::BrowserUnresponsive, detail);
    }
    if method == "session.start" && mode == BrowserMode::Incognito {
        if code == "unsupported" || reason == Some("unsupported_feature") {
            return skip(
                WebfetchBrowserSkipReason::IncognitoUnsupported,
                "the connected browser's extension cannot open an incognito window",
                Some(detail),
            );
        }
        return skip(
            WebfetchBrowserSkipReason::IncognitoUnavailable,
            "the connected browser refused an incognito window",
            Some(detail),
        );
    }
    if code == "timeout" {
        return skip(
            WebfetchBrowserSkipReason::Timeout,
            "the browser read took longer than the request timeout",
            Some(detail),
        );
    }
    skip(
        WebfetchBrowserSkipReason::RenderFailed,
        "the browser read failed",
        Some(detail),
    )
}

/// The session a start call created after it was abandoned, named by this render's own controller name.
async fn abandoned_session(
    sock_path: &str,
    controller_name: &str,
    timeout_ms: u64,
) -> Option<String> {
    if controller_name.is_empty() {
        return None;
    }
    tokio::time::sleep(Duration::from_millis(LATE_SESSION_WAIT_MS)).await;
    let id = make_call_id("system.status");
    let reply = call_daemon(sock_path, &id, "system.status", json!({}), timeout_ms, None)
        .await
        .ok()?;
    let sessions = reply
        .result
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|result| result.get("sessions"))
        .and_then(Value::as_array)?;
    for entry in sessions {
        let Some(session) = entry.as_object() else {
            continue;
        };
        let name = session
            .get("controller")
            .and_then(Value::as_object)
            .and_then(|ctrl| ctrl.get("name"))
            .and_then(Value::as_str);
        if name != Some(controller_name) {
            continue;
        }
        if let Some(session_id) = session
            .get("session_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        {
            return Some(session_id.to_owned());
        }
    }
    None
}

pub struct BrowserSkillRenderer {
    options: BrowserSkillOptions,
    sessions: Arc<Semaphore>,
}

impl BrowserSkillRenderer {
    pub fn new(options: BrowserSkillOptions) -> Self {
        let max_sessions = options.max_sessions.unwrap_or(MAX_SESSIONS);
        Self {
            options,
            sessions: Arc::new(Semaphore::new(max_sessions)),
        }
    }

    async fn render_inner(
        &self,
        url: &str,
        options: PageRenderOptions,
    ) -> Result<RenderedPageOutcome, RenderError> {
        let signal = options.signal.unwrap_or_default();
        if signal.is_aborted() {
            return Err(RenderError::Aborted);
        }
        let settle_poll_ms = self.options.settle_poll_ms.unwrap_or(SETTLE_POLL_MS);
        let status_timeout_ms = self.options.status_timeout_ms.unwrap_or(STATUS_TIMEOUT_MS);
        let deadline = Instant::now() + Duration::from_millis(options.timeout_ms);
        let left_ms = || -> i64 {
            let now = Instant::now();
            if deadline > now {
                (deadline - now).as_millis() as i64
            } else {
                0
            }
        };

        let home = self.options.home.clone().unwrap_or_else(browser_skill_home);
        let sock_path = resolve_daemon_socket(&home, &self.options)?;

        let status_id = make_call_id("system.status");
        let status_budget = status_timeout_ms.min(left_ms().max(1) as u64);
        let status_reply = call_daemon(
            &sock_path,
            &status_id,
            "system.status",
            json!({}),
            status_budget,
            None,
        )
        .await
        .map_err(|error| {
            not_connected(
                WebfetchNotConnectedCause::DaemonUnreachable,
                error.to_string(),
            )
        })?;
        if status_reply.id.as_str() != Some(status_id.as_str()) {
            return Err(not_connected(
                WebfetchNotConnectedCause::Incompatible,
                "status reply id does not match",
            ));
        }
        let status = status_reply
            .result
            .as_ref()
            .and_then(Value::as_object)
            .ok_or_else(|| {
                not_connected(
                    WebfetchNotConnectedCause::Incompatible,
                    "status reply carries no result object",
                )
            })?;
        let proto_ver = status.get("protocol_version");
        if !matches!(protocol_parts(proto_ver), Some((1, _))) {
            let ver_str = match proto_ver {
                Some(Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
                None => "undefined".into(),
            };
            return Err(not_connected(
                WebfetchNotConnectedCause::Incompatible,
                format!("daemon protocol {ver_str} is not 1.x"),
            ));
        }

        let browsers: Vec<&serde_json::Map<String, Value>> = status
            .get("browsers")
            .and_then(Value::as_array)
            .map(|list| list.iter().filter_map(Value::as_object).collect())
            .unwrap_or_default();
        let chosen = pick_browser(&browsers, options.browser.as_deref())?;
        if options.mode == BrowserMode::Incognito && !supports_incognito(chosen.protocol.as_ref()) {
            let ext_ver = match &chosen.protocol {
                Some(Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
                None => "undefined".into(),
            };
            return Err(skip(
                WebfetchBrowserSkipReason::IncognitoUnsupported,
                "the connected browser's extension cannot open an incognito window",
                Some(format!("extension protocol {ext_ver}")),
            ));
        }

        let _permit = tokio::select! {
            biased;
            () = signal.cancelled() => return Err(RenderError::Aborted),
            _ = tokio::time::sleep_until(deadline) => {
                return Err(skip(
                    WebfetchBrowserSkipReason::NoTimeLeft,
                    "no browser session slot was free within the request timeout",
                    None,
                ));
            }
            acquired = Arc::clone(&self.sessions).acquire_owned() => {
                acquired.map_err(|_| skip(
                    WebfetchBrowserSkipReason::RenderFailed,
                    "the browser read failed",
                    Some("session semaphore closed".into()),
                ))?
            }
        };

        if signal.is_aborted() {
            return Err(RenderError::Aborted);
        }

        let mut session_id: Option<String> = None;
        let mut start_called = false;
        let mut start_answered = false;
        let controller_name = format!("senpi webfetch {}", random_hex_4());

        let result = self
            .run_session(
                &sock_path,
                url,
                options.mode,
                &chosen.browser,
                &controller_name,
                settle_poll_ms,
                &signal,
                &left_ms,
                &mut session_id,
                &mut start_called,
                &mut start_answered,
            )
            .await;

        if session_id.is_none() && start_called && !start_answered {
            session_id = abandoned_session(&sock_path, &controller_name, status_timeout_ms).await;
        }
        if let Some(sid) = session_id {
            let stop_id = make_call_id("session.stop");
            let _ = call_daemon(
                &sock_path,
                &stop_id,
                "session.stop",
                json!({ "session_id": sid }),
                STOP_TIMEOUT_MS,
                None,
            )
            .await;
        }

        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_session(
        &self,
        sock_path: &str,
        url: &str,
        mode: BrowserMode,
        chosen: &ConnectedBrowser,
        controller_name: &str,
        settle_poll_ms: u64,
        signal: &AbortSignal,
        left_ms: &(dyn Fn() -> i64 + Send + Sync),
        session_id: &mut Option<String>,
        start_called: &mut bool,
        start_answered: &mut bool,
    ) -> Result<RenderedPageOutcome, RenderError> {
        let settle_reply =
            |method: &str, call_id: &str, reply: Result<DaemonReply, DaemonError>| {
                let method = method.to_owned();
                let call_id = call_id.to_owned();
                async move {
                    match reply {
                        Ok(answered) => {
                            if let Some(err) = answered.error {
                                return Err(daemon_error(&method, mode, &err));
                            }
                            Ok(answered.result.unwrap_or(Value::Null))
                        }
                        Err(DaemonError::Aborted) => Err(RenderError::Aborted),
                        Err(DaemonError::Timeout(msg)) => {
                            if signal.is_aborted() {
                                return Err(RenderError::Aborted);
                            }
                            let cancel_id = make_call_id("cancel");
                            let _ = call_daemon(
                                sock_path,
                                &cancel_id,
                                "cancel",
                                json!({ "rpc_id": call_id }),
                                CANCEL_TIMEOUT_MS,
                                None,
                            )
                            .await;
                            Err(skip(
                                WebfetchBrowserSkipReason::Timeout,
                                "the browser read took longer than the request timeout",
                                Some(msg),
                            ))
                        }
                        Err(DaemonError::Unreachable(msg)) => {
                            if signal.is_aborted() {
                                return Err(RenderError::Aborted);
                            }
                            Err(skip(
                                WebfetchBrowserSkipReason::RenderFailed,
                                "the browser read failed",
                                Some(msg),
                            ))
                        }
                    }
                }
            };

        let call = |method: &str, params: Value| {
            let method = method.to_owned();
            async move {
                if signal.is_aborted() {
                    return Err(RenderError::Aborted);
                }
                let budget_ms = left_ms();
                if budget_ms <= 0 {
                    return Err(skip(
                        WebfetchBrowserSkipReason::Timeout,
                        "the browser read took longer than the request timeout",
                        Some(format!("no time left for {method}")),
                    ));
                }
                let call_id = make_call_id(&method);
                let reply = call_daemon(
                    sock_path,
                    &call_id,
                    &method,
                    params,
                    budget_ms as u64,
                    Some(signal),
                )
                .await;
                settle_reply(&method, &call_id, reply).await
            }
        };

        let start_params = json!({
            "task_name": "webfetch",
            "controller": { "name": controller_name },
            "browser_instance_id": chosen.instance_id,
            "incognito": mode == BrowserMode::Incognito,
            "focused": false,
        });
        if signal.is_aborted() {
            return Err(RenderError::Aborted);
        }
        *start_called = true;
        let start_id = make_call_id("session.start");
        let start_budget = left_ms().max(1) as u64;
        let start_reply = call_daemon(
            sock_path,
            &start_id,
            "session.start",
            start_params,
            start_budget,
            Some(signal),
        )
        .await;
        if let Ok(ref reply) = start_reply {
            if let Some(id) = reply
                .result
                .as_ref()
                .and_then(Value::as_object)
                .and_then(|res| res.get("session_id"))
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
            {
                *session_id = Some(id.to_owned());
            }
            if let Some(err) = &reply.error {
                let code = err.get("code").and_then(Value::as_str);
                if code != Some("timeout") && code != Some("extension_unresponsive") {
                    *start_answered = true;
                }
            }
        }
        let started = settle_reply("session.start", &start_id, start_reply).await?;
        let started_id = started
            .as_object()
            .and_then(|obj| obj.get("session_id"))
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                skip(
                    WebfetchBrowserSkipReason::RenderFailed,
                    "session.start returned no session_id",
                    None,
                )
            })?
            .to_owned();

        let nav_timeout = (left_ms() - GET_HTML_RESERVE_MS as i64).max(1);
        let navigated = call(
            "tool.navigate",
            json!({
                "session_id": started_id,
                "url": url,
                "wait_until": "networkidle",
                "timeout_ms": nav_timeout,
            }),
        )
        .await?;
        let tab_id = navigated
            .as_object()
            .and_then(|obj| obj.get("tab_id"))
            .and_then(Value::as_i64);

        let tab_url = || async {
            let listed = call(
                "tool.tab_list",
                json!({ "session_id": started_id, "scope": "agent" }),
            )
            .await?;
            let tabs: Vec<&serde_json::Map<String, Value>> = listed
                .as_object()
                .and_then(|obj| obj.get("tabs"))
                .and_then(Value::as_array)
                .map(|list| list.iter().filter_map(Value::as_object).collect())
                .unwrap_or_default();
            let tab = match tab_id {
                None => (tabs.len() == 1).then(|| tabs[0]),
                Some(expected) => tabs
                    .into_iter()
                    .find(|entry| entry.get("tab_id").and_then(Value::as_i64) == Some(expected)),
            };
            let Some(url) = tab
                .and_then(|entry| entry.get("url"))
                .and_then(Value::as_str)
                .filter(|url| !url.is_empty())
            else {
                return Err(skip(
                    WebfetchBrowserSkipReason::RenderFailed,
                    "the browser read failed",
                    Some("the session's tab reported no URL".into()),
                ));
            };
            Ok(url.to_owned())
        };

        let mut html = String::new();
        let mut page_url = String::new();
        let mut good: Option<RenderedPage> = None;
        let mut stable_reads = 0usize;
        let mut unsteady_reads = 0usize;
        let mut reads = 0usize;

        loop {
            reads += 1;
            let step: Result<(bool, bool), (RenderError, bool)> = async {
                let before = tab_url().await.map_err(|e| (e, false))?;
                let read = call(
                    "tool.get_html",
                    json!({ "session_id": started_id, "max_bytes": MAX_HTML_BYTES }),
                )
                .await
                .map_err(|e| (e, false))?;
                let after = tab_url().await.map_err(|e| (e, false))?;
                let read_obj = read.as_object();
                let next = read_obj
                    .and_then(|obj| obj.get("html"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let read_tab = read_obj
                    .and_then(|obj| obj.get("tab_id"))
                    .and_then(Value::as_i64);
                let steady = before == after
                    && (tab_id.is_none() || read_tab.is_none() || read_tab == tab_id);
                let next_len = next.encode_utf16().count();
                let prev_len = html.encode_utf16().count();
                stable_reads =
                    if steady && after == page_url && !next.is_empty() && next_len == prev_len {
                        stable_reads + 1
                    } else {
                        0
                    };
                html = next;
                page_url = if steady { after.clone() } else { String::new() };
                unsteady_reads = if steady { 0 } else { unsteady_reads + 1 };
                if steady && !html.is_empty() {
                    good = Some(RenderedPage {
                        url: after,
                        html: html.clone(),
                        truncated: read_obj
                            .and_then(|obj| obj.get("truncated"))
                            .and_then(Value::as_bool)
                            == Some(true),
                    });
                }
                if steady
                    && (stable_reads + 1 >= SETTLED_READS || left_ms() <= settle_poll_ms as i64)
                {
                    return Ok((true, false));
                }
                if !steady && unsteady_reads >= MAX_UNSTEADY_READS {
                    return Err((
                        skip(
                            WebfetchBrowserSkipReason::RenderFailed,
                            "the browser read failed",
                            Some("the page kept navigating while it was read".into()),
                        ),
                        true,
                    ));
                }
                Ok((reads >= MAX_SETTLE_READS, false))
            }
            .await;

            match step {
                Ok((true, _)) => break,
                Ok((false, _)) => {}
                Err((error, is_refusal)) => {
                    if good.is_none() || signal.is_aborted() || is_refusal {
                        return Err(error);
                    }
                    break;
                }
            }

            if settle_poll_ms > 0 {
                tokio::select! {
                    biased;
                    () = signal.cancelled() => return Err(RenderError::Aborted),
                    _ = tokio::time::sleep(Duration::from_millis(settle_poll_ms)) => {}
                }
            } else if signal.is_aborted() {
                return Err(RenderError::Aborted);
            }
        }

        let Some(page) = good else {
            return Err(skip(
                WebfetchBrowserSkipReason::RenderFailed,
                "the browser read failed",
                Some("the page never held still long enough to read".into()),
            ));
        };
        Ok(RenderedPageOutcome {
            page,
            browser: chosen.clone(),
        })
    }
}

impl PageRenderer for BrowserSkillRenderer {
    fn name(&self) -> &str {
        "your connected browser (BrowserSkill)"
    }

    fn render<'a>(&'a self, url: &'a str, options: PageRenderOptions) -> RenderFuture<'a> {
        Box::pin(self.render_inner(url, options))
    }
}

pub fn create_browser_skill_renderer(options: BrowserSkillOptions) -> Arc<dyn PageRenderer> {
    Arc::new(BrowserSkillRenderer::new(options))
}
