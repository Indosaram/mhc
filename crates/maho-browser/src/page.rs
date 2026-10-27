//! One browser tab: navigation, evaluation, accessibility snapshots with element refs, input and
//! screenshots.
//!
//! Port of omowright: `OmOPage`/`Locator`/`OmOMouse`/`OmOKeyboard` in `src/core.js` (navigation via
//! `Page.navigate` + lifecycle events, `Runtime.evaluate` by value, ref locators resolved through the
//! page world's `deref`/`resolvePointerTarget`/`waitForReady`/`checkHitTarget`/`checkEditable`,
//! mouse clicks as `mouseMoved`/`mousePressed`/`mouseReleased`, `keyboard.press`/`type`,
//! `mouse.wheel`, `screenshot`) and `src/page-bundle.js` (the injected snapshot world, vendored as
//! `assets/page-snapshot.js`). Frames, network snooping, downloads, HAR, tracing and the extension
//! bridge are not ported.
use crate::cdp::CdpClient;
use crate::keys;
use base64::Engine as _;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// The injected page world (`globalThis.__mahoPage`).
pub const PAGE_WORLD: &str = include_str!("../assets/page-snapshot.js");
/// omowright `DEFAULT_LOCATOR_WAIT_TIMEOUT_MS`-scale readiness budget for actions.
pub const ACTION_TIMEOUT: Duration = Duration::from_secs(5);
/// Navigation budget.
pub const NAVIGATION_TIMEOUT: Duration = Duration::from_secs(30);

/// When a navigation counts as done.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WaitUntil {
    #[default]
    Load,
    DomContentLoaded,
    NetworkIdle,
    /// Return as soon as the navigation is committed.
    Commit,
}

impl WaitUntil {
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "load" => Some(Self::Load),
            "domcontentloaded" => Some(Self::DomContentLoaded),
            "networkidle" | "networkidle0" | "networkidle2" => Some(Self::NetworkIdle),
            "commit" | "none" => Some(Self::Commit),
            _ => None,
        }
    }

    fn lifecycle_name(self) -> Option<&'static str> {
        match self {
            Self::Load => Some("load"),
            Self::DomContentLoaded => Some("DOMContentLoaded"),
            Self::NetworkIdle => Some("networkIdle"),
            Self::Commit => None,
        }
    }
}

/// Snapshot options passed to the page world's `takeSnapshot`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SnapshotOptions {
    /// Only interactive elements (and their names).
    pub interactive: bool,
    /// CSS selector of the subtree root.
    pub selector: Option<String>,
    /// Ref of the subtree root.
    pub ref_id: Option<String>,
    pub max_depth: Option<u32>,
    pub max_chars: Option<usize>,
}

/// A snapshot: the YAML-like tree with `[ref=eN]` markers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub url: String,
    pub title: String,
    pub tree: String,
    pub refs: usize,
}

/// Where an element sits, in viewport CSS pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct ElementBox {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub tag: String,
}

/// Mouse button names CDP accepts.
pub fn parse_button(value: &str) -> Option<&'static str> {
    match value {
        "left" => Some("left"),
        "right" => Some("right"),
        "middle" => Some("middle"),
        "back" => Some("back"),
        "forward" => Some("forward"),
        _ => None,
    }
}

fn button_mask(button: &str) -> u32 {
    match button {
        "left" => 1,
        "right" => 2,
        "middle" => 4,
        "back" => 8,
        "forward" => 16,
        _ => 0,
    }
}

/// Whether `target` is a snapshot ref (`e12`, `f1e3`, or `[ref=e12]`).
pub fn is_ref(target: &str) -> bool {
    let inner = target
        .strip_prefix("[ref=")
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(target);
    let rest = match inner.strip_prefix('f') {
        Some(after) => {
            let digits = after.chars().take_while(char::is_ascii_digit).count();
            if digits == 0 {
                return false;
            }
            &after[digits..]
        }
        None => inner,
    };
    rest.strip_prefix('e')
        .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
}

const LOCATE: &str = r#"(async (sel, opts) => {
  const P = globalThis.__mahoPage;
  const alias = /^\[ref=((?:f\d+)?e\d+)\]$/.exec(sel);
  const ref = alias ? alias[1] : sel;
  const isRef = /^(f\d+)?e\d+$/.test(ref);
  let el = isRef ? P.deref(ref) : document.querySelector(sel);
  if (!el) return { error: isRef ? `Ref "${ref}" is stale or unknown: the element was removed or the page changed. Take a new snapshot and retry.` : `No element matches selector "${sel}".` };
  if (opts.pointer) el = P.resolvePointerTarget(el) || el;
  el.scrollIntoView({ block: "center", inline: "center", behavior: "instant" });
  const ready = await P.waitForReady(el, opts.checks, opts.timeoutMs);
  if (!ready.ok) return { error: ready.error };
  if (opts.editable) { const e = P.checkEditable(el); if (!e.ok) return { error: e.error }; }
  const r = el.getBoundingClientRect();
  const x = r.left + r.width / 2, y = r.top + r.height / 2;
  if (opts.pointer) { const hit = P.checkHitTarget(el, { x, y }); if (hit && !hit.ok) return { error: hit.error }; }
  if (opts.focus) {
    el.focus();
    if (/^(INPUT|TEXTAREA)$/.test(el.tagName)) { try { el.select(); } catch (_) {} }
    else if (el.isContentEditable) { const s = getSelection(); const range = document.createRange(); range.selectNodeContents(el); s.removeAllRanges(); s.addRange(range); }
  }
  const page = opts.page ? { x: scrollX, y: scrollY } : { x: 0, y: 0 };
  return { x: x + page.x, y: y + page.y, left: r.left + page.x, top: r.top + page.y, width: r.width, height: r.height, tag: el.tagName.toLowerCase() };
})"#;

const SELECT_OPTION: &str = r#"(async (sel, values) => {
  const P = globalThis.__mahoPage;
  const alias = /^\[ref=((?:f\d+)?e\d+)\]$/.exec(sel);
  const ref = alias ? alias[1] : sel;
  const el = /^(f\d+)?e\d+$/.test(ref) ? P.deref(ref) : document.querySelector(sel);
  if (!el) return { error: `No element for "${sel}". Take a new snapshot and retry.` };
  if (el.tagName !== "SELECT") return { error: `Element "${sel}" is a <${el.tagName.toLowerCase()}>, not a <select>.` };
  const options = Array.from(el.options);
  const picked = [];
  for (const value of values) {
    const option = options.find(o => o.value === value) || options.find(o => o.label === value || o.textContent.trim() === value);
    if (!option) return { error: `No option "${value}" in "${sel}". Options: ${options.map(o => JSON.stringify(o.label || o.value)).join(", ")}` };
    picked.push(option);
  }
  if (!el.multiple && picked.length > 1) return { error: "This <select> takes one value." };
  for (const option of options) option.selected = picked.includes(option);
  el.dispatchEvent(new Event("input", { bubbles: true }));
  el.dispatchEvent(new Event("change", { bubbles: true }));
  return { selected: picked.map(o => o.value) };
})"#;

/// One attached tab.
pub struct Page {
    client: CdpClient,
    pub target_id: String,
    pub session_id: String,
    main_frame: Option<String>,
    modifiers: Vec<&'static str>,
    mouse: (f64, f64),
}

impl Page {
    /// Attaches to `target_id` (flattened session) and enables the domains the page uses.
    pub fn attach(client: CdpClient, target_id: &str) -> Result<Self, String> {
        let attached = client.send(
            "Target.attachToTarget",
            json!({"targetId": target_id, "flatten": true}),
            None,
        )?;
        let session_id = attached
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or("Target.attachToTarget returned no sessionId")?
            .to_owned();
        let mut page = Self {
            client,
            target_id: target_id.to_owned(),
            session_id,
            main_frame: None,
            modifiers: Vec::new(),
            mouse: (0.0, 0.0),
        };
        page.send("Page.enable", json!({}))?;
        page.send("Runtime.enable", json!({}))?;
        page.send("Page.setLifecycleEventsEnabled", json!({"enabled": true}))?;
        if let Ok(tree) = page.send("Page.getFrameTree", json!({})) {
            page.main_frame = tree
                .pointer("/frameTree/frame/id")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        Ok(page)
    }

    /// Sends a command on this tab's session.
    pub fn send(&self, method: &str, params: Value) -> Result<Value, String> {
        self.client.send(method, params, Some(&self.session_id))
    }

    pub fn client(&self) -> &CdpClient {
        &self.client
    }

    /// Fixes the viewport size (CSS pixels).
    pub fn set_viewport(&self, width: u32, height: u32, scale: f64) -> Result<(), String> {
        self.send(
            "Emulation.setDeviceMetricsOverride",
            json!({"width": width, "height": height, "deviceScaleFactor": scale, "mobile": false}),
        )
        .map(drop)
    }

    fn wait_lifecycle(
        &self,
        cursor: u64,
        frame: Option<&str>,
        loader: Option<&str>,
        wait: WaitUntil,
        timeout: Duration,
    ) -> bool {
        let Some(name) = wait.lifecycle_name() else {
            return true;
        };
        let session = self.session_id.clone();
        self.client
            .wait_event(cursor, timeout, |event| {
                if event.session_id.as_deref() != Some(session.as_str()) {
                    return false;
                }
                if name == "load" && event.method == "Page.loadEventFired" {
                    return true;
                }
                event.method == "Page.lifecycleEvent"
                    && event.params.get("name").and_then(Value::as_str) == Some(name)
                    && frame.is_none_or(|frame| {
                        event.params.get("frameId").and_then(Value::as_str) == Some(frame)
                    })
                    && loader.is_none_or(|loader| {
                        event.params.get("loaderId").and_then(Value::as_str) == Some(loader)
                    })
            })
            .is_some()
    }

    /// Navigates and waits for `wait`. `Ok(false)` means the page had not settled within
    /// `timeout` (it is still usable).
    pub fn goto(&mut self, url: &str, wait: WaitUntil, timeout: Duration) -> Result<bool, String> {
        let cursor = self.client.event_cursor();
        let navigated = self.send("Page.navigate", json!({"url": url}))?;
        if let Some(error) = navigated
            .get("errorText")
            .and_then(Value::as_str)
            .filter(|error| !error.is_empty())
        {
            return Err(format!("navigation to {url} failed: {error}"));
        }
        let frame = navigated
            .get("frameId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if frame.is_some() {
            self.main_frame.clone_from(&frame);
        }
        let Some(loader) = navigated.get("loaderId").and_then(Value::as_str) else {
            // Same-document navigation (fragment or history API): nothing to wait for.
            return Ok(true);
        };
        Ok(self.wait_lifecycle(cursor, frame.as_deref(), Some(loader), wait, timeout))
    }

    /// Reloads the page.
    pub fn reload(&mut self, wait: WaitUntil, timeout: Duration) -> Result<bool, String> {
        let cursor = self.client.event_cursor();
        self.send("Page.reload", json!({}))?;
        Ok(self.wait_lifecycle(
            cursor,
            self.main_frame.clone().as_deref(),
            None,
            wait,
            timeout,
        ))
    }

    /// Moves `delta` entries through the session history (-1 back, 1 forward).
    pub fn history(
        &mut self,
        delta: i64,
        wait: WaitUntil,
        timeout: Duration,
    ) -> Result<bool, String> {
        let history = self.send("Page.getNavigationHistory", json!({}))?;
        let current = history
            .get("currentIndex")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let entries = history
            .get("entries")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let index = current + delta;
        let Some(entry) = usize::try_from(index)
            .ok()
            .and_then(|index| entries.get(index))
        else {
            return Err(if delta < 0 {
                "There is no previous page in this tab's history.".into()
            } else {
                "There is no next page in this tab's history.".into()
            });
        };
        let id = entry.get("id").cloned().unwrap_or(Value::Null);
        let cursor = self.client.event_cursor();
        self.send("Page.navigateToHistoryEntry", json!({"entryId": id}))?;
        Ok(self.wait_lifecycle(
            cursor,
            self.main_frame.clone().as_deref(),
            None,
            wait,
            timeout,
        ))
    }

    /// Evaluates `expression` (promises awaited) and returns its value by JSON.
    pub fn evaluate(&self, expression: &str) -> Result<Value, String> {
        let result = self.send(
            "Runtime.evaluate",
            json!({"expression": expression, "returnByValue": true, "awaitPromise": true, "userGesture": true}),
        )?;
        if let Some(details) = result.get("exceptionDetails") {
            let description = details
                .pointer("/exception/description")
                .and_then(Value::as_str)
                .or_else(|| details.get("text").and_then(Value::as_str))
                .unwrap_or("evaluation failed");
            return Err(description.to_owned());
        }
        Ok(result
            .pointer("/result/value")
            .cloned()
            .unwrap_or(Value::Null))
    }

    /// Current URL and title.
    pub fn location(&self) -> Result<(String, String), String> {
        let value = self.evaluate("[location.href, document.title]")?;
        let text = |index: usize| {
            value
                .get(index)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        Ok((text(0), text(1)))
    }

    fn ensure_world(&self) -> Result<(), String> {
        let present = self.evaluate(
            "typeof globalThis.__mahoPage === 'object' && globalThis.__mahoPage !== null",
        )?;
        if present == Value::Bool(true) {
            return Ok(());
        }
        self.evaluate(PAGE_WORLD)?;
        Ok(())
    }

    /// Accessibility snapshot with refs usable as action targets.
    pub fn snapshot(&self, options: &SnapshotOptions) -> Result<Snapshot, String> {
        self.ensure_world()?;
        let mut js_options = json!({"interactive": options.interactive});
        if let Some(selector) = &options.selector {
            js_options["selector"] = json!(selector);
        }
        if let Some(ref_id) = &options.ref_id {
            js_options["refId"] = json!(ref_id);
        }
        if let Some(depth) = options.max_depth {
            js_options["maxDepth"] = json!(depth);
        }
        if let Some(chars) = options.max_chars {
            js_options["maxChars"] = json!(chars);
        }
        let result = self.evaluate(&format!(
            "(() => {{ const r = globalThis.__mahoPage.takeSnapshot({js_options}); return {{ tree: r.tree, error: r.error, refs: Object.keys(r.refs || {{}}).length, url: location.href, title: document.title }}; }})()"
        ))?;
        if let Some(error) = result.get("error").and_then(Value::as_str) {
            return Err(error.to_owned());
        }
        let text = |key: &str| {
            result
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        Ok(Snapshot {
            url: text("url"),
            title: text("title"),
            tree: text("tree"),
            refs: result.get("refs").and_then(Value::as_u64).unwrap_or(0) as usize,
        })
    }

    /// Resolves a ref or CSS selector to a ready element (scrolled into view).
    pub fn locate(&self, target: &str, options: Value) -> Result<ElementBox, String> {
        self.ensure_world()?;
        let result = self.evaluate(&format!("{LOCATE}({}, {options})", json!(target)))?;
        if let Some(error) = result.get("error").and_then(Value::as_str) {
            return Err(format!("{target}: {error}"));
        }
        let number = |key: &str| result.get(key).and_then(Value::as_f64).unwrap_or(0.0);
        Ok(ElementBox {
            x: number("x"),
            y: number("y"),
            width: number("width"),
            height: number("height"),
            tag: result
                .get("tag")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        })
    }

    fn mouse_event(
        &self,
        kind: &str,
        x: f64,
        y: f64,
        button: &str,
        buttons: u32,
        clicks: u32,
    ) -> Result<(), String> {
        self.send(
            "Input.dispatchMouseEvent",
            json!({
                "type": kind, "x": x, "y": y, "button": button, "buttons": buttons,
                "clickCount": clicks, "modifiers": keys::modifier_mask(&self.modifiers),
            }),
        )
        .map(drop)
    }

    /// Moves the pointer.
    pub fn mouse_move(&mut self, x: f64, y: f64) -> Result<(), String> {
        self.mouse_event("mouseMoved", x, y, "none", 0, 0)?;
        self.mouse = (x, y);
        Ok(())
    }

    /// Presses or releases a button at the pointer.
    pub fn mouse_button(&mut self, down: bool, button: &str) -> Result<(), String> {
        let (x, y) = self.mouse;
        let mask = button_mask(button);
        self.mouse_event(
            if down {
                "mousePressed"
            } else {
                "mouseReleased"
            },
            x,
            y,
            button,
            if down { mask } else { 0 },
            1,
        )
    }

    /// Clicks `count` times at viewport point (`x`, `y`).
    pub fn mouse_click(&mut self, x: f64, y: f64, button: &str, count: u32) -> Result<(), String> {
        self.mouse_move(x, y)?;
        let mask = button_mask(button);
        for click in 1..=count.max(1) {
            self.mouse_event("mousePressed", x, y, button, mask, click)?;
            self.mouse_event("mouseReleased", x, y, button, 0, click)?;
        }
        Ok(())
    }

    /// Wheel-scrolls at (`x`, `y`) (omowright `mouse.wheel`).
    pub fn mouse_wheel(&mut self, x: f64, y: f64, dx: f64, dy: f64) -> Result<(), String> {
        self.mouse_move(x, y)?;
        self.send(
            "Input.dispatchMouseEvent",
            json!({"type": "mouseWheel", "x": x, "y": y, "deltaX": dx, "deltaY": dy,
                   "modifiers": keys::modifier_mask(&self.modifiers)}),
        )
        .map(drop)
    }

    /// Clicks the element a ref or selector names.
    pub fn click(&mut self, target: &str, button: &str, count: u32) -> Result<ElementBox, String> {
        let element = self.locate(
            target,
            json!({"pointer": true, "checks": ["attached", "visible", "enabled", "stable"], "timeoutMs": ACTION_TIMEOUT.as_millis() as u64}),
        )?;
        self.mouse_click(element.x, element.y, button, count)?;
        Ok(element)
    }

    /// Moves the pointer over the element.
    pub fn hover(&mut self, target: &str) -> Result<ElementBox, String> {
        let element = self.locate(
            target,
            json!({"pointer": true, "checks": ["attached", "visible", "stable"], "timeoutMs": ACTION_TIMEOUT.as_millis() as u64}),
        )?;
        self.mouse_move(element.x, element.y)?;
        Ok(element)
    }

    /// Replaces the element's value with `text` (focus, select all, insert).
    pub fn fill(&mut self, target: &str, text: &str) -> Result<ElementBox, String> {
        let element = self.locate(
            target,
            json!({"editable": true, "focus": true, "checks": ["attached", "visible", "enabled"], "timeoutMs": ACTION_TIMEOUT.as_millis() as u64}),
        )?;
        if text.is_empty() {
            self.press("Delete")?;
        } else {
            self.insert_text(text)?;
        }
        Ok(element)
    }

    /// Picks options of a `<select>` by value or label.
    pub fn select_option(&self, target: &str, values: &[String]) -> Result<Vec<String>, String> {
        self.ensure_world()?;
        let result = self.evaluate(&format!(
            "{SELECT_OPTION}({}, {})",
            json!(target),
            json!(values)
        ))?;
        if let Some(error) = result.get("error").and_then(Value::as_str) {
            return Err(error.to_owned());
        }
        Ok(result
            .get("selected")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Focuses the element (its text is selected, as a click-to-edit would).
    pub fn focus(&self, target: &str) -> Result<ElementBox, String> {
        self.locate(
            target,
            json!({"focus": true, "checks": ["attached"], "timeoutMs": ACTION_TIMEOUT.as_millis() as u64}),
        )
    }

    /// `Input.insertText`.
    pub fn insert_text(&self, text: &str) -> Result<(), String> {
        self.send("Input.insertText", json!({"text": text}))
            .map(drop)
    }

    fn key_event(
        &self,
        down: bool,
        descriptor: &keys::KeyDescriptor,
        auto_repeat: bool,
    ) -> Result<(), String> {
        let kind = if !down {
            "keyUp"
        } else if descriptor.text.is_empty() {
            "rawKeyDown"
        } else {
            "keyDown"
        };
        let mut params = json!({
            "type": kind, "key": descriptor.key, "code": descriptor.code,
            "windowsVirtualKeyCode": descriptor.key_code, "location": descriptor.location,
            "isKeypad": descriptor.location == 3, "autoRepeat": auto_repeat,
            "modifiers": keys::modifier_mask(&self.modifiers),
        });
        if down && !descriptor.text.is_empty() {
            params["text"] = json!(descriptor.text);
            params["unmodifiedText"] = json!(descriptor.text);
        }
        self.send("Input.dispatchKeyEvent", params).map(drop)
    }

    /// Holds a key (modifiers stay held for later input).
    pub fn key_down(&mut self, name: &str) -> Result<(), String> {
        let descriptor = keys::resolve(name, &self.modifiers)?;
        self.key_event(true, &descriptor, false)?;
        if let Some(modifier) = keys::modifier_name(name)
            && !self.modifiers.contains(&modifier)
        {
            self.modifiers.push(modifier);
        }
        Ok(())
    }

    /// Releases a key.
    pub fn key_up(&mut self, name: &str) -> Result<(), String> {
        let descriptor = keys::resolve(name, &self.modifiers)?;
        if let Some(modifier) = keys::modifier_name(name) {
            self.modifiers.retain(|held| *held != modifier);
        }
        self.key_event(false, &descriptor, false)
    }

    /// Presses a key or chord such as `Enter` or `Control+a` (omowright `keyboard.press`).
    pub fn press(&mut self, combo: &str) -> Result<(), String> {
        let parts = keys::split_combo(combo);
        let Some((last, held)) = parts.split_last() else {
            return Err("Key combo is empty".into());
        };
        let mut pressed: Vec<&String> = Vec::new();
        let mut outcome = Ok(());
        for part in held {
            match self.key_down(part) {
                Ok(()) => pressed.push(part),
                Err(error) => {
                    outcome = Err(error);
                    break;
                }
            }
        }
        if outcome.is_ok() {
            outcome = self.key_down(last).and_then(|()| self.key_up(last));
        }
        for part in pressed.into_iter().rev() {
            let released = self.key_up(part);
            if outcome.is_ok() {
                outcome = released;
            }
        }
        outcome
    }

    /// Types `text` key by key; characters without a key (and newlines) are inserted
    /// (omowright `keyboard.type`).
    pub fn type_text(&mut self, text: &str) -> Result<(), String> {
        for c in text.chars() {
            let as_string = c.to_string();
            if c == '\n' || c == '\r' || !keys::is_known(&as_string) {
                self.insert_text(&as_string)?;
                continue;
            }
            let descriptor = keys::resolve(&as_string, &self.modifiers)?;
            self.key_event(true, &descriptor, false)?;
            self.key_event(false, &descriptor, false)?;
        }
        Ok(())
    }

    /// Viewport size in CSS pixels.
    pub fn viewport(&self) -> Result<(f64, f64), String> {
        let metrics = self.send("Page.getLayoutMetrics", json!({}))?;
        let width = metrics
            .pointer("/cssLayoutViewport/clientWidth")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let height = metrics
            .pointer("/cssLayoutViewport/clientHeight")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        Ok((width, height))
    }

    /// Scrolls by (`dx`, `dy`) with the wheel, at the element's centre or the viewport's.
    pub fn scroll(&mut self, target: Option<&str>, dx: f64, dy: f64) -> Result<(), String> {
        let (x, y) = match target {
            Some(target) => {
                let element = self.locate(
                    target,
                    json!({"checks": ["attached"], "timeoutMs": ACTION_TIMEOUT.as_millis() as u64}),
                )?;
                (element.x, element.y)
            }
            None => {
                let (width, height) = self.viewport()?;
                (width / 2.0, height / 2.0)
            }
        };
        self.mouse_wheel(x, y, dx, dy)?;
        std::thread::sleep(Duration::from_millis(150));
        Ok(())
    }

    /// PNG screenshot of the viewport, the whole page, or one element.
    pub fn screenshot(&self, full_page: bool, target: Option<&str>) -> Result<Vec<u8>, String> {
        let mut params = json!({"format": "png"});
        if let Some(target) = target {
            let element = self.locate(
                target,
                json!({"checks": ["attached", "visible"], "page": true, "timeoutMs": ACTION_TIMEOUT.as_millis() as u64}),
            )?;
            params["clip"] = json!({
                "x": element.x - element.width / 2.0, "y": element.y - element.height / 2.0,
                "width": element.width.max(1.0), "height": element.height.max(1.0), "scale": 1,
            });
            params["captureBeyondViewport"] = json!(true);
        } else if full_page {
            let metrics = self.send("Page.getLayoutMetrics", json!({}))?;
            let width = metrics
                .pointer("/cssContentSize/width")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            let height = metrics
                .pointer("/cssContentSize/height")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            if width > 0.0 && height > 0.0 {
                params["clip"] =
                    json!({"x": 0, "y": 0, "width": width, "height": height, "scale": 1});
                params["captureBeyondViewport"] = json!(true);
            }
        }
        let shot = self.send("Page.captureScreenshot", params)?;
        let data = shot
            .get("data")
            .and_then(Value::as_str)
            .ok_or("Page.captureScreenshot returned no data")?;
        base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|error| format!("screenshot is not base64: {error}"))
    }

    /// Waits until a selector/ref is visible, or text appears, or for `duration`.
    pub fn wait_for(
        &self,
        selector: Option<&str>,
        text: Option<&str>,
        timeout: Duration,
    ) -> Result<bool, String> {
        let check = match (selector, text) {
            (Some(selector), _) => {
                self.ensure_world()?;
                format!(
                    "(() => {{ const s = {}; const m = /^\\[ref=((?:f\\d+)?e\\d+)\\]$/.exec(s); const r = m ? m[1] : s; const el = /^(f\\d+)?e\\d+$/.test(r) ? globalThis.__mahoPage.deref(r) : document.querySelector(s); if (!el) return false; const b = el.getBoundingClientRect(); return b.width > 0 && b.height > 0; }})()",
                    json!(selector)
                )
            }
            (None, Some(text)) => format!(
                "!!(document.body && document.body.innerText.includes({}))",
                json!(text)
            ),
            (None, None) => {
                std::thread::sleep(timeout);
                return Ok(true);
            }
        };
        let deadline = Instant::now() + timeout;
        loop {
            // A navigation can wipe the page world between polls; re-inject and retry.
            let found = match self.evaluate(&check) {
                Ok(value) => value == Value::Bool(true),
                Err(_) if selector.is_some() => {
                    let _ = self.ensure_world();
                    false
                }
                Err(error) => return Err(error),
            };
            if found {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refs_are_recognised() {
        for good in ["e1", "e42", "f1e3", "[ref=e7]", "[ref=f2e10]"] {
            assert!(is_ref(good), "{good}");
        }
        for bad in ["e", "#e1", "button", "f1", "fe1", "e1a", "[ref=x]"] {
            assert!(!is_ref(bad), "{bad}");
        }
    }

    #[test]
    fn wait_until_parses_puppeteer_and_playwright_names() {
        assert_eq!(WaitUntil::parse("load"), Some(WaitUntil::Load));
        assert_eq!(
            WaitUntil::parse("DOMContentLoaded"),
            Some(WaitUntil::DomContentLoaded)
        );
        assert_eq!(
            WaitUntil::parse("networkidle2"),
            Some(WaitUntil::NetworkIdle)
        );
        assert_eq!(WaitUntil::parse("commit"), Some(WaitUntil::Commit));
        assert_eq!(WaitUntil::parse("soon"), None);
    }

    #[test]
    fn the_page_world_is_vendored_with_neutral_names() {
        assert!(PAGE_WORLD.contains("globalThis.__mahoPage = {"));
        assert!(PAGE_WORLD.contains("takeSnapshot"));
        assert!(!PAGE_WORLD.contains("__omowright"));
    }

    #[test]
    fn buttons_map_to_masks() {
        assert_eq!(parse_button("right"), Some("right"));
        assert_eq!(parse_button("wheel"), None);
        assert_eq!(button_mask("middle"), 4);
    }
}
