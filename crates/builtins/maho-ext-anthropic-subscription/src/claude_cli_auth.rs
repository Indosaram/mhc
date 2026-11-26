//! Port of senpi `anthropic-subscription/claude-cli-auth.ts` (v2026.11.19-5).
pub use crate::claude_cli_process::ClaudeCliLoginTask;
use crate::{
    claude_cli_account_dir::create_private_account_dir,
    claude_cli_browser_capture::ClaudeCliBrowserCapture,
    claude_cli_process::{ClaudeCliRun, spawn_claude_cli, stop_claude_cli, write_codes},
};
use maho_ai::utils::abort::AbortSignal;
use regex::Regex;
use std::{future::pending, io, sync::LazyLock, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{ChildStderr, ChildStdout},
    sync::mpsc,
};
use url::Url;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaudeCliLoginMethod {
    Browser,
    CopyCode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaudeCliLoginFailure {
    OrgNotAllowed,
    ProviderNotAllowed,
    OAuthFailed,
    UnexpectedExit,
    SpawnFailed {
        kind: io::ErrorKind,
        raw_os_error: Option<i32>,
    },
    IoFailed {
        kind: io::ErrorKind,
        raw_os_error: Option<i32>,
    },
    CleanupFailed {
        kind: io::ErrorKind,
        raw_os_error: Option<i32>,
    },
}

impl ClaudeCliLoginFailure {
    fn io(error: &io::Error) -> Self {
        Self::IoFailed {
            kind: error.kind(),
            raw_os_error: error.raw_os_error(),
        }
    }

    fn cleanup(error: &io::Error) -> Self {
        Self::CleanupFailed {
            kind: error.kind(),
            raw_os_error: error.raw_os_error(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaudeCliLoginResult {
    Success,
    Failed(ClaudeCliLoginFailure),
    Cancelled,
    Timeout,
}

#[derive(PartialEq, Eq)]
pub enum ClaudeCliLoginEvent {
    AutomaticUrl(String),
    ManualUrl(String),
}

pub struct ClaudeCliLoginOptions<'a> {
    pub run: ClaudeCliRun<'a>,
    pub method: ClaudeCliLoginMethod,
    pub signal: Option<AbortSignal>,
    pub idle_timeout_ms: Option<u64>,
    pub kill_grace_ms: Option<u64>,
}

pub struct ClaudeCliLogin {
    pub events: mpsc::UnboundedReceiver<ClaudeCliLoginEvent>,
    pub codes: ClaudeCliCodeSubmitter,
    pub result: ClaudeCliLoginTask,
}

pub struct ClaudeCliCodeSubmitter(mpsc::Sender<String>);

impl ClaudeCliCodeSubmitter {
    /// Queues one line for stdin. The code is never passed via argv or reported as output.
    pub fn submit_code(&self, input: &str) -> bool {
        normalize_pasted_code(input).is_some_and(|line| self.0.try_send(line).is_ok())
    }
}

const LOGIN_IDLE_TIMEOUT_MS: u64 = 10 * 60 * 1_000;
const KILL_GRACE_MS: u64 = 5_000;
const MANUAL_PREFIX: &str = "If the browser didn't open, visit: ";
static TERMINAL_SEQUENCE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b\[[0-?]*[ -/]*[@-~]").ok());

pub fn normalize_pasted_code(input: &str) -> Option<String> {
    let value = input.trim();
    if value.is_empty() || value.contains(['\r', '\n']) {
        return None;
    }
    let url = Url::parse(value);
    let query = match &url {
        Ok(url) => url.query()?,
        Err(_) if !value.contains('#') && value.contains("code=") => {
            value.strip_prefix('?').unwrap_or(value)
        }
        Err(_) => return Some(value.to_owned()),
    };
    let mut params = url::form_urlencoded::parse(query.as_bytes());
    let code = params.clone().find(|(key, _)| key == "code")?.1;
    if code.is_empty() || code.contains(['\r', '\n']) {
        return None;
    }
    let state = params
        .find(|(key, _)| key == "state")
        .map(|(_, value)| value);
    match state {
        Some(state) if state.contains(['\r', '\n']) => None,
        Some(state) if !state.is_empty() => Some(format!("{code}#{state}")),
        _ => Some(code.into_owned()),
    }
}

fn is_claude_sign_in_url(value: &str) -> bool {
    Url::parse(value).is_ok_and(|url| {
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none()
            && matches!(
                url.host_str(),
                Some("claude.ai" | "claude.com" | "platform.claude.com" | "console.anthropic.com")
            )
    })
}

/// Only `--claudeai` is passed: this boundary cannot select a console or SSO billing lane.
pub fn start_claude_cli_login(options: ClaudeCliLoginOptions<'_>) -> ClaudeCliLogin {
    let ClaudeCliLoginOptions {
        run,
        method,
        signal,
        idle_timeout_ms,
        kill_grace_ms,
    } = options;
    let (send_events, events) = mpsc::unbounded_channel();
    let (send_codes, recv_codes) = mpsc::channel(8);
    let started = if signal.as_ref().is_some_and(AbortSignal::aborted) {
        Err(ClaudeCliLoginResult::Cancelled)
    } else {
        (|| {
            let capture = ClaudeCliBrowserCapture::new(method == ClaudeCliLoginMethod::Browser)?;
            create_private_account_dir(&run.config_dir)?;
            let child = spawn_claude_cli(
                &run,
                &["auth", "login", "--claudeai"],
                Some(&capture.helper_path().to_string_lossy()),
            )?;
            Ok::<_, io::Error>((child, capture))
        })()
        .map_err(|error| {
            ClaudeCliLoginResult::Failed(ClaudeCliLoginFailure::SpawnFailed {
                kind: error.kind(),
                raw_os_error: error.raw_os_error(),
            })
        })
    };
    let result = tokio::spawn(async move {
        let (mut child, mut capture) = match started {
            Ok(started) => started,
            Err(result) => return result,
        };
        let (Some(mut stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let error = io::Error::other("CLI pipes unavailable");
            return match stop_claude_cli(
                &mut child,
                Duration::from_millis(kill_grace_ms.unwrap_or(KILL_GRACE_MS)),
            )
            .await
            {
                Ok(()) => ClaudeCliLoginResult::Failed(ClaudeCliLoginFailure::io(&error)),
                Err(error) => ClaudeCliLoginResult::Failed(ClaudeCliLoginFailure::cleanup(&error)),
            };
        };
        let idle = Duration::from_millis(idle_timeout_ms.unwrap_or(LOGIN_IDLE_TIMEOUT_MS));
        let mut recv_codes = recv_codes;
        let result = tokio::select! {
            biased;
            () = async {
                match signal {
                    Some(signal) => signal.cancelled().await,
                    None => pending().await,
                }
            } => ClaudeCliLoginResult::Cancelled,
            result = async {
                tokio::try_join!(child.wait(), observe_output((stdout, stderr), &mut capture, &send_events))
            } => match result {
                Ok((status, output)) if !status.success() => ClaudeCliLoginResult::Failed(output.failure),
                Ok((_, output)) if output.success => ClaudeCliLoginResult::Success,
                Ok(_) => ClaudeCliLoginResult::Failed(ClaudeCliLoginFailure::UnexpectedExit),
                Err(error) => ClaudeCliLoginResult::Failed(ClaudeCliLoginFailure::io(&error)),
            },
            result = write_codes(&mut stdin, &mut recv_codes, idle) => match result {
                Ok(()) => ClaudeCliLoginResult::Timeout,
                Err(error) => ClaudeCliLoginResult::Failed(ClaudeCliLoginFailure::io(&error)),
            },
        };
        recv_codes.close();
        if matches!(
            result,
            ClaudeCliLoginResult::Cancelled
                | ClaudeCliLoginResult::Timeout
                | ClaudeCliLoginResult::Failed(ClaudeCliLoginFailure::IoFailed { .. })
        ) && let Err(error) = stop_claude_cli(
            &mut child,
            Duration::from_millis(kill_grace_ms.unwrap_or(KILL_GRACE_MS)),
        )
        .await
        {
            return ClaudeCliLoginResult::Failed(ClaudeCliLoginFailure::cleanup(&error));
        }
        capture.flush(|url| {
            let _ = send_events.send(ClaudeCliLoginEvent::AutomaticUrl(url));
        });
        result
    });
    ClaudeCliLogin {
        events,
        codes: ClaudeCliCodeSubmitter(send_codes),
        result: ClaudeCliLoginTask(result),
    }
}

struct LoginOutput {
    success: bool,
    failure: ClaudeCliLoginFailure,
}

async fn observe_output(
    (stdout, stderr): (ChildStdout, ChildStderr),
    capture: &mut ClaudeCliBrowserCapture,
    events: &mpsc::UnboundedSender<ClaudeCliLoginEvent>,
) -> io::Result<LoginOutput> {
    let [mut out, mut err] = [Vec::new(), Vec::new()];
    let (mut stdout, mut stderr) = (BufReader::new(stdout), BufReader::new(stderr));
    let (mut out_eof, mut err_eof, mut manual_seen, mut success) = (false, false, false, false);
    let (mut org_denied, mut provider_denied, mut oauth_failed) = (false, false, false);
    while !out_eof || !err_eof {
        tokio::select! {
            read = stdout.read_until(b'\n', &mut out), if !out_eof => {
                out_eof = read? == 0;
                let text = String::from_utf8_lossy(&out);
                let visible = match &*TERMINAL_SEQUENCE {
                    Some(regex) => regex.replace_all(&text, ""),
                    None => std::borrow::Cow::Borrowed(text.as_ref()),
                };
                let line = visible.trim_end_matches(['\r', '\n']);
                success |= matches!(line, "Login successful." | "Paste code here if prompted > Login successful.");
                if !manual_seen && out.ends_with(b"\n")
                    && let Some((_, url)) = line.split_once(MANUAL_PREFIX)
                    && url.starts_with("https://") && !url.is_empty() && !url.contains(char::is_whitespace)
                {
                    manual_seen = true;
                    if is_claude_sign_in_url(url) {
                        let _ = events.send(ClaudeCliLoginEvent::ManualUrl(url.to_owned()));
                    }
                }
                out.clear();
            }
            read = stderr.read_until(b'\n', &mut err), if !err_eof => {
                err_eof = read? == 0;
                let line = String::from_utf8_lossy(&err);
                org_denied |= line.contains("cli_auth_login_org_not_allowed");
                provider_denied |= line.contains("cli_auth_login_provider_not_allowed");
                oauth_failed |= line.contains("OAuth login failed");
                err.clear();
            }
            () = capture.changed() => {}
        }
        capture.flush(|url| {
            let _ = events.send(ClaudeCliLoginEvent::AutomaticUrl(url));
        });
    }
    let failure = if org_denied {
        ClaudeCliLoginFailure::OrgNotAllowed
    } else if provider_denied {
        ClaudeCliLoginFailure::ProviderNotAllowed
    } else if oauth_failed {
        ClaudeCliLoginFailure::OAuthFailed
    } else {
        ClaudeCliLoginFailure::UnexpectedExit
    };
    Ok(LoginOutput { success, failure })
}

#[cfg(all(test, unix))]
#[path = "claude_cli_process_tests.rs"]
mod tests;
