//! The `browser` and `computer` agent tools on the maho extension API.
//!
//! Behaviour follows `docs/cleanroom/specs/browser-tools.md`, written from the MIT sources: the
//! tool surface of oh-my-pi `tools/browser.ts` (open/close/tabs, named tabs with `main` as the
//! default, `wait_until`, viewport, `app.cdp_url`/`app.path`/`app.args`, headed override,
//! dialogs, idle close on session end) and `tools/computer.ts` (screenshot + coordinate actions,
//! close), the Playwright-shaped actions of omowright (`goto`, snapshot refs as action targets,
//! `click`, `fill`, `selectOption`, `press`, `type`, `hover`, `mouse.wheel`, `screenshot`), and
//! the `cua_*` tools of pi-cua-integration (screenshot, click, type, key, scroll, sandbox
//! start/stop/list). oh-my-pi runs its browser through model-written JavaScript in a worker; here
//! each operation is a discrete action, with `evaluate` for arbitrary page JavaScript.
use crate::browser::{Browser, DEFAULT_TAB, LaunchOptions, TransportKind};
use crate::computer::{
    ComputerBackend, DAEMON_STARTUP_TIMEOUT, DesktopDaemon, PageComputer, Point, SandboxRequest,
    default_python, install_daemon_script,
};
use crate::launch::{self, LaunchFeatures};
use crate::page::{SnapshotOptions, WaitUntil};
use base64::Engine as _;
use maho_ext_api::{
    EventKind, EventResult, Extension, ExtensionApi, ToolCall, ToolContent, ToolDefinition,
    ToolError, ToolExecutionMode, ToolResult,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// Tools this extension registers.
pub const TOOL_NAMES: &[&str] = &["browser", "computer"];

/// Snapshot text above this size is cut (the tree stays usable through `selector`/`ref`).
const SNAPSHOT_LIMIT: usize = 60_000;
/// `evaluate` results above this size are cut.
const VALUE_LIMIT: usize = 30_000;

const BROWSER_DESCRIPTION: &str = "Drive a Chromium browser (headless by default) over the DevTools protocol.\n\nTypical loop: `open` a URL, `snapshot` to read the page as an accessibility tree whose interactive elements carry refs like `[ref=e12]`, then act on a ref (or a CSS selector) with `click`, `fill`, `select`, `hover`, `press`, `type` or `scroll`, and `snapshot` again to see the result. Refs come from the latest snapshot; after the page changes, take a new one. Pass `snapshot: true` on an action to get the fresh tree back in the same call.\n\nActions: open (start the browser if needed, open or reuse tab `name`, navigate to `url`), goto, back, forward, reload, snapshot, click, fill (replace a field's value), type (key-by-key into the focused element, or into `target` after focusing it), press (key or chord like `Enter`, `Control+a`), select (pick <select> options by value or label), hover, scroll (`dx`/`dy` pixels, default one screen down), screenshot (viewport, `full_page`, or one `target`; returned as an image and optionally saved to `path`), evaluate (JavaScript expression in the page; promises are awaited; the JSON value is returned), wait (for `target` to be visible, `text` to appear, or `timeout` seconds), tabs (list tabs), close (one tab, or `all: true` to close the browser).\n\nTabs are named (default `main`). JavaScript dialogs are accepted unless `dialogs: \"dismiss\"`. The browser stays open across calls until `close` or the end of the session.";

const COMPUTER_DESCRIPTION: &str = "Coordinate-level control of a screen: take a screenshot, then click, move, drag, scroll, type or press keys at pixel positions read from it.\n\nBackends: `browser` (the viewport of a browser tab opened with the browser tool; coordinates are CSS pixels of the screenshot) and `desktop` (the host desktop or a Cua sandbox through a Python daemon; needs Python 3.12/3.13 with `pip install cua`). The default is `browser` when a browser tab is open, else `desktop`.\n\nActions: screenshot, click (`button`, `clicks` 1-3, `modifiers` held during the click), double_click, move, drag (`path` of at least two points), scroll (`dx`/`dy` at `x`,`y`), type (`text`), key (`keys`: a chord like `ctrl+s` or a list of chords), and for the desktop backend sandbox_start (`mode` local|cloud, `os`, `runtime`, `kind`, `version`, `region`; cloud reads CUA_API_KEY), sandbox_stop, sandbox_list, status. Prefer the browser tool's refs for web pages; use this when the page or app has no usable accessibility tree (canvas, custom widgets, native apps).";

/// Host settings for the tools.
#[derive(Clone, Debug)]
pub struct BrowserExtension {
    pub launch: LaunchOptions,
    /// Existing DevTools endpoint to attach to instead of launching (`$MAHO_BROWSER_CDP_URL`).
    pub cdp_url: Option<String>,
    /// Python for the desktop daemon.
    pub python: String,
    /// Where the desktop daemon script is written.
    pub daemon_dir: PathBuf,
}

fn env_var(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn falsy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "0" | "false" | "no" | "off"
    )
}

impl BrowserExtension {
    /// Settings from the environment: `$MAHO_BROWSER_PATH` (and the other executable variables),
    /// `$MAHO_BROWSER_HEADLESS` (default on), `$MAHO_BROWSER_CDP_URL`, `$MAHO_BROWSER_TRANSPORT`
    /// (`pipe` | `port`), `$MAHO_COMPUTER_PYTHON`.
    pub fn from_env() -> Self {
        let mut launch = LaunchOptions {
            executable: launch::find_executable(&env_var),
            ..LaunchOptions::default()
        };
        if let Some(headless) = env_var("MAHO_BROWSER_HEADLESS") {
            launch.headless = !falsy(&headless);
        }
        if env_var("MAHO_BROWSER_TRANSPORT").is_some_and(|value| value.eq_ignore_ascii_case("port"))
        {
            launch.transport = TransportKind::Port;
        }
        Self {
            launch,
            cdp_url: env_var("MAHO_BROWSER_CDP_URL"),
            python: default_python(&env_var),
            daemon_dir: std::env::temp_dir().join("maho-computer"),
        }
    }
}

/// Tool state for one session.
pub struct Session {
    settings: BrowserExtension,
    cwd: PathBuf,
    browser: Option<Browser>,
    desktop: Option<DesktopDaemon>,
}

impl Session {
    pub fn new(settings: BrowserExtension, cwd: &Path) -> Self {
        Self {
            settings,
            cwd: cwd.to_path_buf(),
            browser: None,
            desktop: None,
        }
    }

    /// Closes the browser and the desktop daemon.
    pub fn close_all(&mut self) {
        if let Some(mut browser) = self.browser.take() {
            browser.close();
        }
        if let Some(mut desktop) = self.desktop.take() {
            desktop.shutdown();
        }
    }

    fn browser_or_start(&mut self, params: &Value) -> Result<&mut Browser, String> {
        if self
            .browser
            .as_ref()
            .is_some_and(|browser| !browser.is_connected())
        {
            self.browser = None;
        }
        if self.browser.is_none() {
            let app = params.get("app").cloned().unwrap_or(Value::Null);
            let cdp_url = str_param(&app, "cdp_url").or_else(|| self.settings.cdp_url.clone());
            let browser = match cdp_url {
                Some(url) => Browser::connect(&url)?,
                None => {
                    let mut options = self.settings.launch.clone();
                    if let Some(path) = str_param(&app, "path") {
                        options.executable = Some(resolve_path(&self.cwd, &path));
                    }
                    if let Some(args) = app.get("args").and_then(Value::as_array) {
                        options.args = args
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect();
                    }
                    if let Some(headed) = params.get("headed").and_then(Value::as_bool) {
                        options.headless = !headed;
                    }
                    if let Some(viewport) = params.get("viewport") {
                        let width = viewport.get("width").and_then(Value::as_u64);
                        let height = viewport.get("height").and_then(Value::as_u64);
                        if let (Some(width), Some(height)) = (width, height) {
                            options.viewport = (
                                width.clamp(200, 4096) as u32,
                                height.clamp(200, 4096) as u32,
                            );
                        }
                    }
                    options.features = LaunchFeatures {
                        ignore_https_errors: bool_param(params, "ignore_https_errors"),
                        allow_file_access: bool_param(params, "allow_file_access"),
                    };
                    Browser::launch(&options)?
                }
            };
            self.browser = Some(browser);
        }
        if let (Some(browser), Some(dialogs)) = (&self.browser, str_param(params, "dialogs")) {
            browser.client().set_accept_dialogs(dialogs != "dismiss");
        }
        self.browser
            .as_mut()
            .ok_or_else(|| "browser unavailable".into())
    }

    fn desktop_or_start(&mut self) -> Result<&mut DesktopDaemon, String> {
        if self.desktop.is_none() {
            let script = install_daemon_script(&self.settings.daemon_dir)?;
            self.desktop = Some(DesktopDaemon::start(
                &self.settings.python,
                &script,
                DAEMON_STARTUP_TIMEOUT,
            )?);
        }
        self.desktop
            .as_mut()
            .ok_or_else(|| "desktop daemon unavailable".into())
    }
}

fn str_param(params: &Value, key: &str) -> Option<String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn bool_param(params: &Value, key: &str) -> bool {
    params.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn number_param(params: &Value, key: &str) -> Option<f64> {
    params.get(key).and_then(Value::as_f64)
}

fn required(params: &Value, key: &str, action: &str) -> Result<String, String> {
    str_param(params, key).ok_or_else(|| format!("`{action}` needs `{key}`"))
}

fn resolve_path(cwd: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

fn timeout_param(params: &Value, default: Duration) -> Duration {
    number_param(params, "timeout")
        .filter(|seconds| *seconds > 0.0)
        .map_or(default, |seconds| {
            Duration::from_secs_f64(seconds.min(300.0))
        })
}

fn clip(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut cut = limit;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!(
        "{}\n… [truncated {} of {} chars; narrow with `selector`, `ref` or `max_depth`]",
        &text[..cut],
        text.len() - cut,
        text.len()
    )
}

/// What a tool call produced.
#[derive(Debug, Default, PartialEq)]
pub struct Output {
    pub text: String,
    pub image_png: Option<Vec<u8>>,
    pub details: Value,
}

impl Output {
    fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            image_png: None,
            details: Value::Null,
        }
    }

    fn into_result(self) -> ToolResult {
        let mut content = Vec::new();
        if let Some(png) = self.image_png {
            content.push(ToolContent::Image {
                data: base64::engine::general_purpose::STANDARD.encode(png),
                mime_type: "image/png".into(),
            });
        }
        content.push(ToolContent::text(self.text));
        ToolResult {
            content,
            details: (!self.details.is_null()).then_some(self.details),
            structured_content: None,
            is_error: None,
        }
    }
}

fn settle_note(settled: bool) -> &'static str {
    if settled { "" } else { " (still loading)" }
}

/// Runs one `browser` action.
pub fn run_browser(session: &mut Session, params: &Value) -> Result<Output, String> {
    let action = required(params, "action", "browser")?;
    let name = str_param(params, "name").unwrap_or_else(|| DEFAULT_TAB.to_owned());
    let wait = match str_param(params, "wait_until") {
        Some(value) => WaitUntil::parse(&value).ok_or_else(|| {
            format!(
                "unknown wait_until {value:?}: use load, domcontentloaded, networkidle or commit"
            )
        })?,
        None => WaitUntil::Load,
    };
    match action.as_str() {
        "tabs" => {
            let Some(browser) = session.browser.as_ref() else {
                return Ok(Output::text("No browser is open."));
            };
            let targets = browser.targets()?;
            let mut text = format!("{} tab(s):", targets.len());
            for target in &targets {
                text.push_str(&format!(
                    "\n- {}: {} {}",
                    target.name.as_deref().unwrap_or("(not opened here)"),
                    target.url,
                    if target.title.is_empty() {
                        String::new()
                    } else {
                        format!("— {}", target.title)
                    }
                ));
            }
            Ok(Output {
                text,
                image_png: None,
                details: json!({"tabs": targets.iter().map(|t| json!({"name": t.name, "url": t.url, "title": t.title, "id": t.id})).collect::<Vec<_>>()}),
            })
        }
        "close" => {
            if bool_param(params, "all") {
                let had = session.browser.is_some();
                if let Some(mut browser) = session.browser.take() {
                    browser.close();
                }
                return Ok(Output::text(if had {
                    "Browser closed."
                } else {
                    "No browser is open."
                }));
            }
            let Some(browser) = session.browser.as_mut() else {
                return Ok(Output::text("No browser is open."));
            };
            if browser.close_tab(&name)? {
                Ok(Output::text(format!("Closed tab {name}.")))
            } else {
                Ok(Output::text(format!(
                    "No tab named {name}. Open tabs: {}",
                    browser.tab_names().join(", ")
                )))
            }
        }
        "open" => {
            let url = str_param(params, "url");
            let browser = session.browser_or_start(params)?;
            if let Some(target) = str_param(params.get("app").unwrap_or(&Value::Null), "target")
                && browser.tab(&name).is_none()
            {
                let found = browser
                    .targets()?
                    .into_iter()
                    .find(|info| {
                        info.id == target
                            || info.url.contains(&target)
                            || info.title.contains(&target)
                    })
                    .ok_or_else(|| format!("no open page matches {target:?}"))?;
                browser.adopt(&name, &found.id)?;
            }
            let settled = browser.open(&name, url.as_deref(), wait)?;
            after_action(
                session,
                params,
                &name,
                &format!("Tab {name} open{}", settle_note(settled)),
            )
        }
        _ => {
            let has_tab = session
                .browser
                .as_mut()
                .is_some_and(|browser| browser.tab(&name).is_some());
            if !has_tab {
                if action == "goto" {
                    // `goto` without `open` opens the tab first.
                    session.browser_or_start(params)?.open(&name, None, wait)?;
                } else if session.browser.is_none() {
                    return Err(format!(
                        "No browser tab {name:?} is open. Call the browser tool with action \"open\" first."
                    ));
                } else {
                    let names = session
                        .browser
                        .as_ref()
                        .map(Browser::tab_names)
                        .unwrap_or_default();
                    return Err(format!(
                        "No tab named {name:?}. Open tabs: {}",
                        names.join(", ")
                    ));
                }
            }
            page_action(session, params, &action, &name, wait)
        }
    }
}

fn page_action(
    session: &mut Session,
    params: &Value,
    action: &str,
    name: &str,
    wait: WaitUntil,
) -> Result<Output, String> {
    let cwd = session.cwd.clone();
    let browser = session.browser.as_mut().ok_or("No browser is open.")?;
    let page = browser
        .tab(name)
        .ok_or_else(|| format!("No tab named {name:?}."))?;
    let timeout = timeout_param(params, crate::page::NAVIGATION_TIMEOUT);
    let message = match action {
        "goto" => {
            let url = required(params, "url", action)?;
            let settled = page.goto(&url, wait, timeout)?;
            format!("Navigated to {url}{}", settle_note(settled))
        }
        "back" => format!("Went back{}", settle_note(page.history(-1, wait, timeout)?)),
        "forward" => format!(
            "Went forward{}",
            settle_note(page.history(1, wait, timeout)?)
        ),
        "reload" => format!("Reloaded{}", settle_note(page.reload(wait, timeout)?)),
        "snapshot" => {
            let snapshot = page.snapshot(&SnapshotOptions {
                interactive: bool_param(params, "interactive"),
                selector: str_param(params, "selector"),
                ref_id: str_param(params, "ref"),
                max_depth: params
                    .get("max_depth")
                    .and_then(Value::as_u64)
                    .map(|depth| depth as u32),
                max_chars: None,
            })?;
            let text = format!(
                "Page: {} — {}\n{} ref(s)\n{}",
                snapshot.url,
                snapshot.title,
                snapshot.refs,
                clip(&snapshot.tree, SNAPSHOT_LIMIT)
            );
            return Ok(Output {
                text,
                image_png: None,
                details: json!({"url": snapshot.url, "title": snapshot.title, "refs": snapshot.refs}),
            });
        }
        "click" => {
            let button = str_param(params, "button").unwrap_or_else(|| "left".into());
            let button = crate::page::parse_button(&button)
                .ok_or_else(|| format!("unknown button {button:?}"))?;
            let count = params
                .get("count")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .clamp(1, 3) as u32;
            match (
                str_param(params, "target"),
                number_param(params, "x"),
                number_param(params, "y"),
            ) {
                (Some(target), _, _) => {
                    let element = page.click(&target, button, count)?;
                    format!(
                        "Clicked {target} (<{}>) at ({:.0}, {:.0})",
                        element.tag, element.x, element.y
                    )
                }
                (None, Some(x), Some(y)) => {
                    page.mouse_click(x, y, button, count)?;
                    format!("Clicked at ({x:.0}, {y:.0})")
                }
                _ => {
                    return Err(
                        "`click` needs `target` (a ref or CSS selector) or `x` and `y`".into(),
                    );
                }
            }
        }
        "hover" => {
            let target = required(params, "target", action)?;
            page.hover(&target)?;
            format!("Hovering {target}")
        }
        "fill" => {
            let target = required(params, "target", action)?;
            let text = params
                .get("text")
                .and_then(Value::as_str)
                .ok_or("`fill` needs `text`")?;
            page.fill(&target, text)?;
            format!("Filled {target} ({} chars)", text.chars().count())
        }
        "type" => {
            let text = params
                .get("text")
                .and_then(Value::as_str)
                .ok_or("`type` needs `text`")?;
            if let Some(target) = str_param(params, "target") {
                page.focus(&target)?;
            }
            page.type_text(text)?;
            format!("Typed {} chars", text.chars().count())
        }
        "press" => {
            let keys = required(params, "keys", action)?;
            page.press(&keys)?;
            format!("Pressed {keys}")
        }
        "select" => {
            let target = required(params, "target", action)?;
            let values: Vec<String> = match params.get("values") {
                Some(Value::Array(values)) => values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
                Some(Value::String(value)) => vec![value.clone()],
                _ => str_param(params, "text").into_iter().collect(),
            };
            if values.is_empty() {
                return Err("`select` needs `values`".into());
            }
            let selected = page.select_option(&target, &values)?;
            format!("Selected {} in {target}", selected.join(", "))
        }
        "scroll" => {
            let dx = number_param(params, "dx").unwrap_or(0.0);
            let dy = number_param(params, "dy").unwrap_or(if dx == 0.0 { 600.0 } else { 0.0 });
            page.scroll(str_param(params, "target").as_deref(), dx, dy)?;
            format!("Scrolled by ({dx:.0}, {dy:.0})")
        }
        "screenshot" => {
            let png = page.screenshot(
                bool_param(params, "full_page"),
                str_param(params, "target").as_deref(),
            )?;
            let (width, height) = crate::computer::png_dimensions(&png)?;
            let mut text = format!("Screenshot {width}x{height}");
            if let Some(path) = str_param(params, "path") {
                let path = resolve_path(&cwd, &path);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                }
                std::fs::write(&path, &png)
                    .map_err(|error| format!("cannot save {}: {error}", path.display()))?;
                text.push_str(&format!(", saved to {}", path.display()));
            }
            return Ok(Output {
                text,
                image_png: Some(png),
                details: json!({"width": width, "height": height}),
            });
        }
        "evaluate" => {
            let expression = params
                .get("expression")
                .or_else(|| params.get("code"))
                .and_then(Value::as_str)
                .ok_or("`evaluate` needs `expression`")?;
            let value = page.evaluate(expression)?;
            let rendered = match &value {
                Value::String(text) => text.clone(),
                other => serde_json::to_string_pretty(other).unwrap_or_default(),
            };
            return Ok(Output {
                text: clip(&rendered, VALUE_LIMIT),
                image_png: None,
                details: json!({"type": match value { Value::Null => "null", Value::Bool(_) => "boolean", Value::Number(_) => "number", Value::String(_) => "string", Value::Array(_) => "array", Value::Object(_) => "object" }}),
            });
        }
        "wait" => {
            let target = str_param(params, "target");
            let text = str_param(params, "text");
            let budget = timeout_param(
                params,
                if target.is_some() || text.is_some() {
                    Duration::from_secs(10)
                } else {
                    Duration::from_secs(1)
                },
            );
            let found = page.wait_for(target.as_deref(), text.as_deref(), budget)?;
            match (target, text, found) {
                (None, None, _) => format!("Waited {:.1}s", budget.as_secs_f64()),
                (_, _, true) => "Condition met".to_owned(),
                (_, _, false) => {
                    return Err(format!(
                        "Timed out after {:.1}s waiting",
                        budget.as_secs_f64()
                    ));
                }
            }
        }
        other => {
            return Err(format!(
                "unknown browser action {other:?}: use open, goto, back, forward, reload, snapshot, click, fill, type, press, select, hover, scroll, screenshot, evaluate, wait, tabs or close"
            ));
        }
    };
    after_action(session, params, name, &message)
}

/// Appends the location, and the fresh snapshot when `snapshot: true`.
fn after_action(
    session: &mut Session,
    params: &Value,
    name: &str,
    message: &str,
) -> Result<Output, String> {
    let page = session
        .browser
        .as_mut()
        .and_then(|browser| browser.tab(name))
        .ok_or_else(|| format!("No tab named {name:?}."))?;
    let (url, title) = page.location().unwrap_or_default();
    let mut text = format!(
        "{message}.\nPage: {url}{}",
        if title.is_empty() {
            String::new()
        } else {
            format!(" — {title}")
        }
    );
    if bool_param(params, "snapshot") {
        match page.snapshot(&SnapshotOptions {
            interactive: bool_param(params, "interactive"),
            ..SnapshotOptions::default()
        }) {
            Ok(snapshot) => text.push_str(&format!(
                "\n{} ref(s)\n{}",
                snapshot.refs,
                clip(&snapshot.tree, SNAPSHOT_LIMIT)
            )),
            Err(error) => text.push_str(&format!("\n(snapshot failed: {error})")),
        }
    }
    Ok(Output {
        text,
        image_png: None,
        details: json!({"tab": name, "url": url, "title": title}),
    })
}

fn point(params: &Value) -> Result<Point, String> {
    match (number_param(params, "x"), number_param(params, "y")) {
        (Some(x), Some(y)) => Ok(Point { x, y }),
        _ => Err("this action needs `x` and `y`".into()),
    }
}

fn string_list(params: &Value, key: &str) -> Vec<String> {
    match params.get(key) {
        Some(Value::String(value)) => vec![value.clone()],
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

fn computer_actions(
    backend: &mut dyn ComputerBackend,
    action: &str,
    params: &Value,
) -> Result<Output, String> {
    let label = backend.label();
    let text = match action {
        "screenshot" => {
            let shot = backend.screenshot()?;
            return Ok(Output {
                text: format!("Screenshot {}x{} ({label})", shot.width, shot.height),
                details: json!({"width": shot.width, "height": shot.height, "backend": label}),
                image_png: Some(shot.png),
            });
        }
        "click" | "double_click" => {
            let at = point(params)?;
            let button = str_param(params, "button").unwrap_or_else(|| "left".into());
            if !matches!(button.as_str(), "left" | "right" | "middle") {
                return Err(format!("unknown button {button:?}"));
            }
            let clicks = if action == "double_click" {
                2
            } else {
                params
                    .get("clicks")
                    .and_then(Value::as_u64)
                    .unwrap_or(1)
                    .clamp(1, 3) as u32
            };
            backend.click(at, &button, clicks, &string_list(params, "modifiers"))?;
            format!(
                "{button} click x{clicks} at ({:.0}, {:.0}) on {label}",
                at.x, at.y
            )
        }
        "move" => {
            let at = point(params)?;
            backend.move_to(at)?;
            format!("Moved to ({:.0}, {:.0}) on {label}", at.x, at.y)
        }
        "drag" => {
            let path: Vec<Point> = params
                .get("path")
                .and_then(Value::as_array)
                .map(|points| points.iter().filter_map(|p| point(p).ok()).collect())
                .unwrap_or_default();
            backend.drag(&path)?;
            format!("Dragged through {} points on {label}", path.len())
        }
        "scroll" => {
            let at = point(params)?;
            let dx = number_param(params, "dx").unwrap_or(0.0);
            let dy = number_param(params, "dy").unwrap_or(0.0);
            backend.scroll(at, dx, dy)?;
            format!(
                "Scrolled ({dx:.0}, {dy:.0}) at ({:.0}, {:.0}) on {label}",
                at.x, at.y
            )
        }
        "type" => {
            let text = params
                .get("text")
                .and_then(Value::as_str)
                .ok_or("`type` needs `text`")?;
            backend.type_text(text)?;
            format!("Typed {} chars on {label}", text.chars().count())
        }
        "key" => {
            let chords = string_list(params, "keys");
            if chords.is_empty() {
                return Err("`key` needs `keys`".into());
            }
            backend.keys(&chords)?;
            format!("Pressed {} on {label}", chords.join(", "))
        }
        other => {
            return Err(format!(
                "unknown computer action {other:?}: use screenshot, click, double_click, move, drag, scroll, type, key, sandbox_start, sandbox_stop, sandbox_list or status"
            ));
        }
    };
    Ok(Output::text(text))
}

/// Runs one `computer` action.
pub fn run_computer(session: &mut Session, params: &Value) -> Result<Output, String> {
    let action = required(params, "action", "computer")?;
    let name = str_param(params, "name").unwrap_or_else(|| DEFAULT_TAB.to_owned());
    let has_tab = session
        .browser
        .as_mut()
        .is_some_and(|browser| browser.tab(&name).is_some());
    let backend = str_param(params, "backend")
        .unwrap_or_else(|| if has_tab { "browser" } else { "desktop" }.to_owned());
    match (backend.as_str(), action.as_str()) {
        ("browser", "status" | "sandbox_start" | "sandbox_stop" | "sandbox_list") => Err(format!(
            "`{action}` is a desktop backend action; pass backend: \"desktop\""
        )),
        ("browser", _) => {
            let page = session
                .browser
                .as_mut()
                .and_then(|browser| browser.tab(&name))
                .ok_or_else(|| format!("No browser tab {name:?} is open. Open one with the browser tool, or use backend: \"desktop\"."))?;
            computer_actions(&mut PageComputer { page }, &action, params)
        }
        ("desktop", "status") => {
            let daemon = session.desktop_or_start()?;
            let ready = daemon.ready.clone();
            let label = daemon.label();
            let sandboxes = daemon.list_sandboxes().unwrap_or_default();
            Ok(Output {
                text: format!(
                    "Desktop daemon {} ({}); cua {}; target: {label}; {} sandbox(es)",
                    ready.version,
                    session.settings.python,
                    match (&ready.cua_version, ready.cua_available) {
                        (Some(version), true) => format!("{version} available"),
                        (None, true) => "available".to_owned(),
                        _ => format!(
                            "unavailable ({})",
                            ready.cua_import_error.as_deref().unwrap_or("not installed")
                        ),
                    },
                    sandboxes.len()
                ),
                image_png: None,
                details: json!({"cuaAvailable": ready.cua_available, "target": label, "sandboxes": sandboxes}),
            })
        }
        ("desktop", "sandbox_start") => {
            let daemon = session.desktop_or_start()?;
            let mode = str_param(params, "mode").unwrap_or_else(|| "local".into());
            let api_key = if mode == "cloud" {
                env_var("CUA_API_KEY")
            } else {
                None
            };
            if mode == "cloud" && api_key.is_none() {
                return Err("cloud sandboxes need CUA_API_KEY in the environment".into());
            }
            let name = daemon.start_sandbox(&SandboxRequest {
                mode,
                name: str_param(params, "sandbox"),
                os: str_param(params, "os").unwrap_or_else(|| "linux".into()),
                version: str_param(params, "version"),
                kind: str_param(params, "kind"),
                runtime: str_param(params, "runtime"),
                api_key,
                region: str_param(params, "region"),
            })?;
            Ok(Output::text(format!(
                "Sandbox {name} started; it is now the desktop target."
            )))
        }
        ("desktop", "sandbox_stop") => {
            let daemon = session.desktop_or_start()?;
            let name = match str_param(params, "sandbox") {
                Some(name) => name,
                None => match &daemon.target {
                    crate::computer::DesktopTarget::Sandbox(name) => name.clone(),
                    crate::computer::DesktopTarget::Localhost => {
                        return Err("`sandbox_stop` needs `sandbox`".into());
                    }
                },
            };
            daemon.stop_sandbox(&name)?;
            Ok(Output::text(format!("Sandbox {name} stopped.")))
        }
        ("desktop", "sandbox_list") => {
            let sandboxes = session.desktop_or_start()?.list_sandboxes()?;
            let mut text = format!("{} sandbox(es)", sandboxes.len());
            for sandbox in &sandboxes {
                text.push_str(&format!(
                    "\n- {} ({}, {}, {})",
                    sandbox.get("name").and_then(Value::as_str).unwrap_or("?"),
                    sandbox.get("mode").and_then(Value::as_str).unwrap_or("?"),
                    sandbox
                        .get("os_type")
                        .and_then(Value::as_str)
                        .unwrap_or("?"),
                    sandbox.get("status").and_then(Value::as_str).unwrap_or("?"),
                ));
            }
            Ok(Output {
                text,
                image_png: None,
                details: json!({"sandboxes": sandboxes}),
            })
        }
        ("desktop", _) => {
            let daemon = session.desktop_or_start()?;
            if let Some(sandbox) = str_param(params, "sandbox") {
                daemon.target = crate::computer::DesktopTarget::Sandbox(sandbox);
            }
            computer_actions(daemon, &action, params)
        }
        (other, _) => Err(format!(
            "unknown computer backend {other:?}: use browser or desktop"
        )),
    }
}

fn browser_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": {"type": "string", "enum": ["open", "goto", "back", "forward", "reload", "snapshot", "click", "fill", "type", "press", "select", "hover", "scroll", "screenshot", "evaluate", "wait", "tabs", "close"], "description": "operation"},
            "name": {"type": "string", "description": "tab name (default 'main')"},
            "url": {"type": "string", "description": "URL for open/goto"},
            "target": {"type": "string", "description": "element: a snapshot ref like e12 or a CSS selector"},
            "text": {"type": "string", "description": "text for fill/type, or text to wait for"},
            "keys": {"type": "string", "description": "key or chord for press, e.g. Enter, Control+a"},
            "values": {"type": "array", "items": {"type": "string"}, "description": "option values or labels for select"},
            "button": {"type": "string", "enum": ["left", "right", "middle"]},
            "count": {"type": "integer", "description": "click count (1-3)"},
            "x": {"type": "number", "description": "viewport x for a coordinate click"},
            "y": {"type": "number", "description": "viewport y for a coordinate click"},
            "dx": {"type": "number", "description": "horizontal scroll in pixels"},
            "dy": {"type": "number", "description": "vertical scroll in pixels (default 600)"},
            "expression": {"type": "string", "description": "JavaScript expression for evaluate"},
            "interactive": {"type": "boolean", "description": "snapshot only interactive elements"},
            "selector": {"type": "string", "description": "snapshot only this CSS subtree"},
            "ref": {"type": "string", "description": "snapshot only this ref's subtree"},
            "max_depth": {"type": "integer", "description": "snapshot depth limit"},
            "snapshot": {"type": "boolean", "description": "return a fresh snapshot after the action"},
            "full_page": {"type": "boolean", "description": "screenshot the whole page"},
            "path": {"type": "string", "description": "also save the screenshot PNG here (cwd-relative)"},
            "wait_until": {"type": "string", "enum": ["load", "domcontentloaded", "networkidle", "commit"], "description": "navigation wait condition"},
            "timeout": {"type": "number", "description": "timeout in seconds"},
            "dialogs": {"type": "string", "enum": ["accept", "dismiss"], "description": "how JavaScript dialogs are answered"},
            "headed": {"type": "boolean", "description": "show the browser window (first open only)"},
            "viewport": {"type": "object", "properties": {"width": {"type": "number"}, "height": {"type": "number"}}, "required": ["width", "height"]},
            "ignore_https_errors": {"type": "boolean"},
            "allow_file_access": {"type": "boolean"},
            "app": {"type": "object", "description": "browser to use (first open only)", "properties": {
                "path": {"type": "string", "description": "browser executable to launch"},
                "cdp_url": {"type": "string", "description": "existing DevTools endpoint (http://host:port or ws://...)"},
                "args": {"type": "array", "items": {"type": "string"}, "description": "extra browser switches"},
                "target": {"type": "string", "description": "URL/title substring or id of an existing page to drive"}
            }},
            "all": {"type": "boolean", "description": "close: close the whole browser"}
        },
        "required": ["action"]
    })
}

fn computer_schema() -> Value {
    let point = json!({"type": "object", "properties": {"x": {"type": "number"}, "y": {"type": "number"}}, "required": ["x", "y"]});
    json!({
        "type": "object",
        "properties": {
            "action": {"type": "string", "enum": ["screenshot", "click", "double_click", "move", "drag", "scroll", "type", "key", "sandbox_start", "sandbox_stop", "sandbox_list", "status"]},
            "backend": {"type": "string", "enum": ["browser", "desktop"], "description": "default: browser when a browser tab is open, else desktop"},
            "name": {"type": "string", "description": "browser tab (default 'main')"},
            "x": {"type": "number"},
            "y": {"type": "number"},
            "button": {"type": "string", "enum": ["left", "right", "middle"]},
            "clicks": {"type": "integer", "description": "1-3"},
            "modifiers": {"type": "array", "items": {"type": "string"}, "description": "keys held during a click (browser backend)"},
            "path": {"type": "array", "items": point, "description": "drag path, at least two points"},
            "dx": {"type": "number"},
            "dy": {"type": "number"},
            "text": {"type": "string"},
            "keys": {"description": "chord like ctrl+s, or a list of chords", "anyOf": [{"type": "string"}, {"type": "array", "items": {"type": "string"}}]},
            "sandbox": {"type": "string", "description": "sandbox name"},
            "mode": {"type": "string", "enum": ["local", "cloud"]},
            "os": {"type": "string", "enum": ["linux", "macos", "windows", "android"]},
            "runtime": {"type": "string", "enum": ["auto", "docker", "qemu", "lume", "tart"]},
            "kind": {"type": "string", "enum": ["vm", "container"]},
            "version": {"type": "string"},
            "region": {"type": "string"}
        },
        "required": ["action"]
    })
}

type Runner = fn(&mut Session, &Value) -> Result<Output, String>;

async fn run_blocking(
    session: Arc<Mutex<Session>>,
    params: Value,
    runner: Runner,
) -> Result<ToolResult, ToolError> {
    let job = move || {
        let mut session = session.lock().unwrap_or_else(PoisonError::into_inner);
        runner(&mut session, &params).map(Output::into_result)
    };
    let outcome = match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle
            .spawn_blocking(job)
            .await
            .unwrap_or_else(|error| Err(format!("browser task failed: {error}"))),
        Err(_) => job(),
    };
    outcome.map_err(ToolError::Message)
}

fn define(
    session: &Arc<Mutex<Session>>,
    name: &str,
    label: &str,
    description: &str,
    parameters: Value,
    runner: Runner,
) -> ToolDefinition {
    let session = Arc::clone(session);
    let mut definition = ToolDefinition::new(
        name,
        description,
        parameters,
        Arc::new(move |call: ToolCall<'_>| {
            let session = Arc::clone(&session);
            let params = call.params.clone();
            Box::pin(run_blocking(session, params, runner))
        }),
    );
    label.clone_into(&mut definition.label);
    definition.execution_mode = Some(ToolExecutionMode::Sequential);
    definition
}

/// The tool definitions over one session.
pub fn tool_definitions(session: &Arc<Mutex<Session>>) -> Vec<ToolDefinition> {
    vec![
        define(
            session,
            "browser",
            "Browser",
            BROWSER_DESCRIPTION,
            browser_schema(),
            run_browser,
        ),
        define(
            session,
            "computer",
            "Computer",
            COMPUTER_DESCRIPTION,
            computer_schema(),
            run_computer,
        ),
    ]
}

impl Extension for BrowserExtension {
    fn register(&self, api: &mut ExtensionApi) {
        let session = Arc::new(Mutex::new(Session::new(self.clone(), &api.cwd)));
        for definition in tool_definitions(&session) {
            if let Err(error) = api.try_register_tool(definition) {
                maho_ext_api::diagnostic(&format!("browser: {error}"));
            }
        }
        api.on(
            EventKind::SessionShutdown,
            Arc::new(move |_event, _ctx| {
                let session = Arc::clone(&session);
                Box::pin(async move {
                    let close = move || {
                        session
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .close_all()
                    };
                    match tokio::runtime::Handle::try_current() {
                        Ok(handle) => {
                            let _ = handle.spawn_blocking(close).await;
                        }
                        Err(_) => close(),
                    }
                    Ok(EventResult::None)
                })
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        Session::new(
            BrowserExtension {
                launch: LaunchOptions {
                    executable: Some(PathBuf::from("/nonexistent/chromium")),
                    ..LaunchOptions::default()
                },
                cdp_url: None,
                python: "python3".into(),
                daemon_dir: std::env::temp_dir().join("maho-computer-test"),
            },
            Path::new("/tmp"),
        )
    }

    #[test]
    fn actions_without_a_browser_explain_what_to_do() {
        let mut session = session();
        assert_eq!(
            run_browser(&mut session, &json!({"action": "tabs"})).map(|o| o.text),
            Ok("No browser is open.".into())
        );
        assert_eq!(
            run_browser(&mut session, &json!({"action": "close", "all": true})).map(|o| o.text),
            Ok("No browser is open.".into())
        );
        let error = run_browser(&mut session, &json!({"action": "click", "target": "e1"}))
            .err()
            .unwrap_or_default();
        assert!(error.contains("action \"open\" first"), "{error}");
        assert!(run_browser(&mut session, &json!({})).is_err());
        let launch = run_browser(
            &mut session,
            &json!({"action": "open", "url": "about:blank"}),
        )
        .err()
        .unwrap_or_default();
        assert!(launch.contains("/nonexistent/chromium"), "{launch}");
        let bad_wait = run_browser(
            &mut session,
            &json!({"action": "goto", "url": "x", "wait_until": "soon"}),
        )
        .err()
        .unwrap_or_default();
        assert!(bad_wait.contains("wait_until"), "{bad_wait}");
    }

    #[test]
    fn computer_browser_backend_needs_a_tab() {
        let mut session = session();
        let error = run_computer(
            &mut session,
            &json!({"action": "screenshot", "backend": "browser"}),
        )
        .err()
        .unwrap_or_default();
        assert!(error.contains("No browser tab"), "{error}");
        let error = run_computer(
            &mut session,
            &json!({"action": "status", "backend": "browser"}),
        )
        .err()
        .unwrap_or_default();
        assert!(error.contains("desktop backend action"), "{error}");
        assert!(
            run_computer(&mut session, &json!({"action": "click", "backend": "moon"})).is_err()
        );
    }

    #[test]
    fn clip_marks_truncation() {
        assert_eq!(clip("short", 10), "short");
        let clipped = clip(&"é".repeat(20), 7);
        assert!(clipped.starts_with("ééé\n… [truncated"), "{clipped}");
    }

    #[test]
    fn schemas_list_every_action() {
        let browser = browser_schema();
        let actions = browser
            .pointer("/properties/action/enum")
            .and_then(Value::as_array)
            .map(Vec::len);
        assert_eq!(actions, Some(18));
        let computer = computer_schema();
        assert_eq!(computer.pointer("/required/0"), Some(&json!("action")));
    }

    #[test]
    fn outputs_put_the_image_first() {
        let result = Output {
            text: "shot".into(),
            image_png: Some(vec![1, 2, 3]),
            details: json!({"w": 1}),
        }
        .into_result();
        assert!(
            matches!(result.content.first(), Some(ToolContent::Image { mime_type, .. }) if mime_type == "image/png")
        );
        assert_eq!(result.content.len(), 2);
        assert_eq!(result.details, Some(json!({"w": 1})));
    }
}
