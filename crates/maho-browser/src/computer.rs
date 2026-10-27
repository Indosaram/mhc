//! Computer use: coordinate-level screen control behind one trait, with two backends.
//!
//! - [`PageComputer`]: the viewport of a browser tab. Port of omowright `src/cua.js` (`createCua`:
//!   click, double click, drag, keypress, move, scroll, type and visible screenshot over the page's
//!   mouse and keyboard, modifiers held around pointer actions).
//! - [`DesktopDaemon`]: the host desktop or a Cua sandbox through a Python daemon. Port of
//!   pi-cua-integration `src/cua/daemon.ts` (newline-delimited JSON-RPC over stdio, ready event,
//!   per-call timeout, shutdown) and `src/cua/client.ts` (method names, parameter and result
//!   shapes, target encoding). The daemon itself is the vendored `python/daemon.py`
//!   (`assets/desktop-daemon.py`); it needs Python 3.12/3.13 with the `cua` package, so this
//!   backend is best effort and reports what is missing.
use crate::page::Page;
use base64::Engine as _;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// The vendored desktop daemon.
pub const DESKTOP_DAEMON: &str = include_str!("../assets/desktop-daemon.py");

/// A PNG screenshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Screenshot {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

impl Screenshot {
    /// Reads the size from the PNG header.
    pub fn from_png(png: Vec<u8>) -> Result<Self, String> {
        let (width, height) = png_dimensions(&png)?;
        Ok(Self { png, width, height })
    }

    pub fn base64(&self) -> String {
        base64::engine::general_purpose::STANDARD.encode(&self.png)
    }
}

/// Width and height from a PNG's IHDR chunk (pi-cua-integration `_png_dimensions`).
pub fn png_dimensions(png: &[u8]) -> Result<(u32, u32), String> {
    if png.len() < 24 || !png.starts_with(b"\x89PNG\r\n\x1a\n") || &png[12..16] != b"IHDR" {
        return Err("Screenshot bytes are not a PNG image.".into());
    }
    let width = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
    let height = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);
    Ok((width, height))
}

/// A point in screen (or viewport) pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// Coordinate-level control of one screen.
pub trait ComputerBackend {
    /// Short label for results (`browser`, `desktop`, `sandbox <name>`).
    fn label(&self) -> String;
    fn screenshot(&mut self) -> Result<Screenshot, String>;
    /// `button` is `left`, `right` or `middle`; `clicks` 1–3.
    fn click(&mut self, at: Point, button: &str, clicks: u32, modifiers: &[String]) -> Result<(), String>;
    fn move_to(&mut self, at: Point) -> Result<(), String>;
    fn drag(&mut self, path: &[Point]) -> Result<(), String>;
    fn scroll(&mut self, at: Point, dx: f64, dy: f64) -> Result<(), String>;
    fn type_text(&mut self, text: &str) -> Result<(), String>;
    /// Key chords such as `Enter` or `ctrl+s`, pressed in order.
    fn keys(&mut self, chords: &[String]) -> Result<(), String>;
}

/// The viewport of a browser tab as a screen.
pub struct PageComputer<'a> {
    pub page: &'a mut Page,
}

impl PageComputer<'_> {
    fn hold<T>(&mut self, modifiers: &[String], action: impl FnOnce(&mut Page) -> Result<T, String>) -> Result<T, String> {
        let held: Vec<&String> = modifiers
            .iter()
            .filter(|key| crate::keys::modifier_name(key).is_some())
            .collect();
        let mut pressed = Vec::new();
        let mut outcome = Ok(());
        for key in &held {
            match self.page.key_down(key) {
                Ok(()) => pressed.push(*key),
                Err(error) => {
                    outcome = Err(error);
                    break;
                }
            }
        }
        let result = outcome.and_then(|()| action(self.page));
        for key in pressed.into_iter().rev() {
            let _ = self.page.key_up(key);
        }
        result
    }
}

impl ComputerBackend for PageComputer<'_> {
    fn label(&self) -> String {
        "browser".into()
    }

    fn screenshot(&mut self) -> Result<Screenshot, String> {
        Screenshot::from_png(self.page.screenshot(false, None)?)
    }

    fn click(&mut self, at: Point, button: &str, clicks: u32, modifiers: &[String]) -> Result<(), String> {
        self.hold(modifiers, |page| page.mouse_click(at.x, at.y, button, clicks))
    }

    fn move_to(&mut self, at: Point) -> Result<(), String> {
        self.page.mouse_move(at.x, at.y)
    }

    fn drag(&mut self, path: &[Point]) -> Result<(), String> {
        let [first, rest @ ..] = path else {
            return Err("drag requires a path with at least two points".into());
        };
        if rest.is_empty() {
            return Err("drag requires a path with at least two points".into());
        }
        self.page.mouse_move(first.x, first.y)?;
        self.page.mouse_button(true, "left")?;
        let mut outcome = Ok(());
        for point in rest {
            outcome = self.page.mouse_move(point.x, point.y);
            if outcome.is_err() {
                break;
            }
        }
        let released = self.page.mouse_button(false, "left");
        outcome.and(released)
    }

    fn scroll(&mut self, at: Point, dx: f64, dy: f64) -> Result<(), String> {
        self.page.mouse_wheel(at.x, at.y, dx, dy)
    }

    fn type_text(&mut self, text: &str) -> Result<(), String> {
        self.page.type_text(text)
    }

    fn keys(&mut self, chords: &[String]) -> Result<(), String> {
        for chord in chords {
            self.page.press(chord)?;
        }
        Ok(())
    }
}

/// Where desktop actions run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DesktopTarget {
    /// The host machine, unsandboxed (Cua `Localhost`).
    Localhost,
    /// A sandbox started through the daemon.
    Sandbox(String),
}

impl DesktopTarget {
    /// pi-cua-integration `encodeTarget`.
    fn encode(&self) -> Value {
        match self {
            Self::Localhost => json!({"target_kind": "localhost"}),
            Self::Sandbox(name) => json!({"target_kind": "sandbox", "target_name": name}),
        }
    }
}

/// The daemon's `ready` event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DaemonReady {
    pub version: String,
    pub cua_available: bool,
    pub cua_version: Option<String>,
    pub cua_import_error: Option<String>,
}

/// Sandbox parameters for `start_sandbox`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SandboxRequest {
    /// `local` or `cloud`.
    pub mode: String,
    pub name: Option<String>,
    /// `linux`, `macos`, `windows`, `android`.
    pub os: String,
    pub version: Option<String>,
    /// `vm` or `container`.
    pub kind: Option<String>,
    /// `auto`, `docker`, `qemu`, `lume`, `tart`.
    pub runtime: Option<String>,
    pub api_key: Option<String>,
    pub region: Option<String>,
}

#[derive(Default)]
struct DaemonState {
    responses: HashMap<u64, Result<Value, String>>,
    ready: Option<DaemonReady>,
    exited: Option<String>,
    stderr: String,
}

/// A running desktop daemon.
pub struct DesktopDaemon {
    child: Child,
    stdin: ChildStdin,
    state: Arc<(Mutex<DaemonState>, Condvar)>,
    next_id: u64,
    pub ready: DaemonReady,
    pub target: DesktopTarget,
    request_timeout: Duration,
}

/// pi-cua-integration defaults.
pub const DAEMON_STARTUP_TIMEOUT: Duration = Duration::from_secs(20);
pub const DAEMON_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

fn parse_ready(value: &Value) -> Option<DaemonReady> {
    (value.get("type").and_then(Value::as_str) == Some("ready")).then(|| DaemonReady {
        version: value.get("version").and_then(Value::as_str).unwrap_or_default().to_owned(),
        cua_available: value.get("cuaAvailable").and_then(Value::as_bool).unwrap_or(false),
        cua_version: value.get("cuaVersion").and_then(Value::as_str).map(str::to_owned),
        cua_import_error: value.get("cuaImportError").and_then(Value::as_str).map(str::to_owned),
    })
}

/// Writes the vendored daemon to `dir` (content-addressed, so concurrent sessions share it).
pub fn install_daemon_script(dir: &Path) -> Result<PathBuf, String> {
    let digest = DESKTOP_DAEMON
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        });
    let path = dir.join(format!("maho-desktop-daemon-{digest:016x}.py"));
    if std::fs::read_to_string(&path).is_ok_and(|existing| existing == DESKTOP_DAEMON) {
        return Ok(path);
    }
    std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    let staging = dir.join(format!("{}.{}.tmp", path.display(), std::process::id()));
    std::fs::write(&staging, DESKTOP_DAEMON)
        .and_then(|()| std::fs::rename(&staging, &path))
        .map_err(|error| format!("cannot write the desktop daemon: {error}"))?;
    Ok(path)
}

/// The Python interpreter: `$MAHO_COMPUTER_PYTHON`, else `python3` (`python` on Windows).
pub fn default_python(env: &dyn Fn(&str) -> Option<String>) -> String {
    env("MAHO_COMPUTER_PYTHON")
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| if cfg!(windows) { "python" } else { "python3" }.to_owned())
}

impl DesktopDaemon {
    /// Starts `python -u script` and waits for its ready event.
    pub fn start(python: &str, script: &Path, startup_timeout: Duration) -> Result<Self, String> {
        let mut child = Command::new(python)
            .arg("-u")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("cannot start the desktop daemon with {python}: {error}"))?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            return Err("desktop daemon pipes are unavailable".into());
        };
        let state: Arc<(Mutex<DaemonState>, Condvar)> = Arc::default();
        let errors = Arc::clone(&state);
        let _ = std::thread::Builder::new().name("desktop-daemon-stderr".into()).spawn(move || {
            let mut text = String::new();
            let mut reader = BufReader::new(stderr);
            let _ = reader.read_to_string(&mut text);
            let (lock, changed) = &*errors;
            lock.lock().unwrap_or_else(PoisonError::into_inner).stderr = text;
            changed.notify_all();
        });
        let replies = Arc::clone(&state);
        let _ = std::thread::Builder::new().name("desktop-daemon-stdout".into()).spawn(move || {
            let (lock, changed) = &*replies;
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|read| read > 0) {
                if let Ok(message) = serde_json::from_str::<Value>(line.trim()) {
                    let mut state = lock.lock().unwrap_or_else(PoisonError::into_inner);
                    if let Some(ready) = parse_ready(&message) {
                        state.ready = Some(ready);
                    } else if let Some(id) = message.get("id").and_then(Value::as_u64) {
                        let response = match message.get("error") {
                            Some(error) => Err(format!(
                                "[desktop daemon {}] {}",
                                error.get("code").and_then(Value::as_i64).unwrap_or(0),
                                error.get("message").and_then(Value::as_str).unwrap_or("error")
                            )),
                            None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                        };
                        state.responses.insert(id, response);
                    }
                    changed.notify_all();
                }
                line.clear();
            }
            let mut state = lock.lock().unwrap_or_else(PoisonError::into_inner);
            state.exited = Some("the desktop daemon exited".into());
            changed.notify_all();
        });
        let deadline = Instant::now() + startup_timeout;
        let ready = {
            let (lock, changed) = &*state;
            let mut guard = lock.lock().unwrap_or_else(PoisonError::into_inner);
            loop {
                if let Some(ready) = guard.ready.clone() {
                    break Ok(ready);
                }
                if guard.exited.is_some() {
                    // Give the stderr reader a moment to collect the traceback.
                    let (next, _) = changed
                        .wait_timeout(guard, Duration::from_millis(200))
                        .unwrap_or_else(PoisonError::into_inner);
                    guard = next;
                    break Err(format!(
                        "the desktop daemon exited before it was ready{}",
                        if guard.stderr.trim().is_empty() {
                            String::new()
                        } else {
                            format!(": {}", guard.stderr.trim())
                        }
                    ));
                }
                let now = Instant::now();
                if now >= deadline {
                    break Err(format!(
                        "the desktop daemon did not signal ready within {} ms",
                        startup_timeout.as_millis()
                    ));
                }
                guard = changed
                    .wait_timeout(guard, deadline - now)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
        };
        match ready {
            Ok(ready) => Ok(Self {
                child,
                stdin,
                state,
                next_id: 1,
                ready,
                target: DesktopTarget::Localhost,
                request_timeout: DAEMON_REQUEST_TIMEOUT,
            }),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(error)
            }
        }
    }

    /// Sends one request and waits for its response.
    pub fn call(&mut self, method: &str, params: Value, timeout: Option<Duration>) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let line = json!({"id": id, "method": method, "params": params}).to_string();
        let (lock, changed) = &*self.state;
        if let Some(exited) = &lock.lock().unwrap_or_else(PoisonError::into_inner).exited {
            return Err(exited.clone());
        }
        writeln!(self.stdin, "{line}")
            .and_then(|()| self.stdin.flush())
            .map_err(|error| format!("desktop daemon stdin is not writable: {error}"))?;
        let timeout = timeout.unwrap_or(self.request_timeout);
        let deadline = Instant::now() + timeout;
        let mut guard = lock.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(response) = guard.responses.remove(&id) {
                return response;
            }
            if let Some(exited) = &guard.exited {
                let stderr = guard.stderr.trim();
                return Err(if stderr.is_empty() {
                    exited.clone()
                } else {
                    format!("{exited}: {stderr}")
                });
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(format!(
                    "desktop daemon call timed out after {} ms: {method}",
                    timeout.as_millis()
                ));
            }
            guard = changed
                .wait_timeout(guard, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    fn call_target(&mut self, method: &str, extra: Value) -> Result<Value, String> {
        let mut params = self.target.encode();
        if let (Some(params), Value::Object(extra)) = (params.as_object_mut(), extra) {
            params.extend(extra);
        }
        self.call(method, params, None)
    }

    pub fn ping(&mut self) -> Result<String, String> {
        let result = self.call("ping", json!({}), None)?;
        Ok(result
            .get("daemon_version")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned())
    }

    /// Starts a sandbox and makes it the target. Returns its name.
    pub fn start_sandbox(&mut self, request: &SandboxRequest) -> Result<String, String> {
        let result = self.call(
            "start_sandbox",
            json!({
                "mode": request.mode, "name": request.name, "os": request.os, "version": request.version,
                "kind": request.kind, "runtime": request.runtime, "api_key": request.api_key, "region": request.region,
            }),
            Some(Duration::from_secs(600)),
        )?;
        let name = result
            .get("name")
            .and_then(Value::as_str)
            .ok_or("Invalid desktop daemon response for start_sandbox: expected string field 'name'")?
            .to_owned();
        self.target = DesktopTarget::Sandbox(name.clone());
        Ok(name)
    }

    /// Destroys a sandbox; the target falls back to the host when it was this one.
    pub fn stop_sandbox(&mut self, name: &str) -> Result<(), String> {
        self.call("stop_sandbox", json!({"name": name}), Some(Duration::from_secs(120)))?;
        if self.target == DesktopTarget::Sandbox(name.to_owned()) {
            self.target = DesktopTarget::Localhost;
        }
        Ok(())
    }

    /// Active sandboxes (name, mode, os, status).
    pub fn list_sandboxes(&mut self) -> Result<Vec<Value>, String> {
        let result = self.call("list_sandboxes", json!({}), None)?;
        Ok(result
            .get("sandboxes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// Runs a shell command on the target.
    pub fn shell(&mut self, command: &str, timeout: Option<Duration>) -> Result<(String, String, i64), String> {
        let mut params = self.target.encode();
        params["command"] = json!(command);
        params["timeout_ms"] = json!(timeout.map(|timeout| timeout.as_millis() as u64));
        let result = self.call("shell", params, timeout.map(|timeout| timeout + Duration::from_secs(5)))?;
        Ok((
            result.get("stdout").and_then(Value::as_str).unwrap_or_default().to_owned(),
            result.get("stderr").and_then(Value::as_str).unwrap_or_default().to_owned(),
            result.get("exit_code").and_then(Value::as_i64).unwrap_or(0),
        ))
    }

    /// Asks the daemon to stop, then kills it after a grace period.
    pub fn shutdown(&mut self) {
        let _ = writeln!(self.stdin, "{}", json!({"id": 0, "method": "shutdown", "params": {}}));
        let _ = self.stdin.flush();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for DesktopDaemon {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            self.shutdown();
        }
    }
}

impl ComputerBackend for DesktopDaemon {
    fn label(&self) -> String {
        match &self.target {
            DesktopTarget::Localhost => "desktop".into(),
            DesktopTarget::Sandbox(name) => format!("sandbox {name}"),
        }
    }

    fn screenshot(&mut self) -> Result<Screenshot, String> {
        let result = self.call_target("screenshot", json!({}))?;
        let data = result
            .get("png_b64")
            .and_then(Value::as_str)
            .ok_or("Invalid desktop daemon response for screenshot: expected string field 'png_b64'")?;
        let png = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|error| format!("screenshot is not base64: {error}"))?;
        let mut shot = Screenshot::from_png(png)?;
        if let (Some(width), Some(height)) = (
            result.get("width").and_then(Value::as_u64).filter(|width| *width > 0),
            result.get("height").and_then(Value::as_u64).filter(|height| *height > 0),
        ) {
            shot.width = width as u32;
            shot.height = height as u32;
        }
        Ok(shot)
    }

    fn click(&mut self, at: Point, button: &str, clicks: u32, modifiers: &[String]) -> Result<(), String> {
        if !modifiers.is_empty() {
            return Err("the desktop backend does not hold modifiers during a click; press a key chord instead".into());
        }
        self.call_target(
            "click",
            json!({"x": at.x.round() as i64, "y": at.y.round() as i64, "button": button, "clicks": clicks.max(1)}),
        )
        .map(drop)
    }

    fn move_to(&mut self, _at: Point) -> Result<(), String> {
        Err("the desktop daemon has no pointer-move method".into())
    }

    fn drag(&mut self, _path: &[Point]) -> Result<(), String> {
        Err("the desktop daemon has no drag method".into())
    }

    fn scroll(&mut self, at: Point, dx: f64, dy: f64) -> Result<(), String> {
        self.call_target(
            "scroll",
            json!({"x": at.x.round() as i64, "y": at.y.round() as i64, "scroll_x": dx.round() as i64, "scroll_y": dy.round() as i64}),
        )
        .map(drop)
    }

    fn type_text(&mut self, text: &str) -> Result<(), String> {
        self.call_target("type", json!({"text": text})).map(drop)
    }

    fn keys(&mut self, chords: &[String]) -> Result<(), String> {
        self.call_target("key", json!({"keys": chords})).map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_dimensions_come_from_ihdr() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&640u32.to_be_bytes());
        png.extend_from_slice(&480u32.to_be_bytes());
        assert_eq!(png_dimensions(&png), Ok((640, 480)));
        assert!(png_dimensions(b"GIF89a").is_err());
    }

    #[test]
    fn targets_encode_like_the_client() {
        assert_eq!(DesktopTarget::Localhost.encode(), json!({"target_kind": "localhost"}));
        assert_eq!(
            DesktopTarget::Sandbox("sb".into()).encode(),
            json!({"target_kind": "sandbox", "target_name": "sb"})
        );
    }

    #[test]
    fn ready_events_parse() {
        let ready = parse_ready(&json!({"type": "ready", "version": "0.1.0", "cuaAvailable": false, "cuaVersion": null, "cuaImportError": "ModuleNotFoundError"}));
        assert_eq!(
            ready,
            Some(DaemonReady {
                version: "0.1.0".into(),
                cua_available: false,
                cua_version: None,
                cua_import_error: Some("ModuleNotFoundError".into()),
            })
        );
        assert_eq!(parse_ready(&json!({"type": "log"})), None);
    }

    #[test]
    fn python_defaults_and_override() {
        assert_eq!(default_python(&|_| Some("/opt/py".into())), "/opt/py");
        let fallback = default_python(&|_| None);
        assert!(fallback == "python3" || fallback == "python");
    }

    #[test]
    fn the_daemon_is_vendored() {
        assert!(DESKTOP_DAEMON.contains("def handle_screenshot"));
        assert!(DESKTOP_DAEMON.contains("Vendored from code-yeongyu/pi-cua-integration"));
    }
}
