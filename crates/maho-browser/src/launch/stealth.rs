//! Stealth patches for automated pages.
//!
//! Port of the "Stealth patches" section of oh-my-pi `packages/coding-agent/src/tools/browser/launch.ts`
//! (`stealthIgnoreDefaultArgs`, `isMicrosoftEdgeExecutable`, `resolveUserAgentOverride`,
//! `sendUserAgentOverride`, `buildStealthInjectionScript`, `applyStealthPatches`) with the
//! fourteen patch scripts of `tools/puppeteer/*_stealth_*.txt` vendored unchanged under
//! `assets/stealth/` and the injection wrapper as `assets/stealth/injection.js`.
//!
//! `configureUserAgentTargets` is ported as [`configure_user_agent_targets`]: browser-wide
//! `Target.setAutoAttach` (flattened) and a listener thread that sends the override to every
//! attached page, webview or background page (popups included), plus a one-off override through
//! a temporary session for each such target that already exists.
//!
//! PORT-DIVERGENCE: puppeteer's `page.evaluateOnNewDocument` is
//! `Page.addScriptToEvaluateOnNewDocument` on the page session. The puppeteer `CDPSession` event
//! listener is a thread draining a [`crate::cdp::Subscription`]; it stops when the
//! [`StealthState`] is dropped or the connection closes. The Rust launcher never adds
//! puppeteer's default switches, so [`stealth_ignore_default_args`] documents what is left out
//! rather than filtering a list.
use crate::cdp::{CdpClient, SubscriptionCancel};
use serde_json::{Value, json};
use std::thread::JoinHandle;
use std::time::Duration;

const ENABLE_AUTOMATION_FLAG: &str = "--enable-automation";
/// oh-my-pi `STEALTH_IGNORE_DEFAULT_ARGS`.
const STEALTH_IGNORE_DEFAULT_ARGS: [&str; 9] = [
    ENABLE_AUTOMATION_FLAG,
    "--disable-extensions",
    "--disable-default-apps",
    "--disable-component-extensions-with-background-pages",
    "--disable-popup-blocking",
    "--disable-client-side-phishing-detection",
    "--allow-pre-commit-input",
    "--disable-ipc-flooding-protection",
    "--metrics-recording-only",
];
/// Launch arg that keeps `navigator.webdriver` false.
pub const DISABLE_AUTOMATION_CONTROLLED_ARG: &str = "--disable-blink-features=AutomationControlled";
const STEALTH_ACCEPT_LANGUAGE: &str = "en-US,en";
const USER_AGENT_TARGET_TYPES: [&str; 3] = ["page", "webview", "background_page"];
/// oh-my-pi `USER_AGENT_TARGET_TIMEOUT_MS`: each per-target override is best effort.
pub const USER_AGENT_TARGET_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the target listener sleeps between liveness checks when nothing arrives; a cancel
/// or disconnect wakes it at once.
const LISTENER_IDLE_WAIT: Duration = Duration::from_secs(3600);

/// The patch scripts, in injection order (`STEALTH_PATCH_SCRIPTS`).
pub const STEALTH_PATCH_SCRIPTS: [&str; 14] = [
    include_str!("../../assets/stealth/00_stealth_tampering.txt"),
    include_str!("../../assets/stealth/01_stealth_activity.txt"),
    include_str!("../../assets/stealth/02_stealth_hairline.txt"),
    include_str!("../../assets/stealth/03_stealth_botd.txt"),
    include_str!("../../assets/stealth/04_stealth_iframe.txt"),
    include_str!("../../assets/stealth/05_stealth_webgl.txt"),
    include_str!("../../assets/stealth/06_stealth_screen.txt"),
    include_str!("../../assets/stealth/07_stealth_fonts.txt"),
    include_str!("../../assets/stealth/08_stealth_audio.txt"),
    include_str!("../../assets/stealth/09_stealth_locale.txt"),
    include_str!("../../assets/stealth/10_stealth_plugins.txt"),
    include_str!("../../assets/stealth/11_stealth_hardware.txt"),
    include_str!("../../assets/stealth/12_stealth_codecs.txt"),
    include_str!("../../assets/stealth/13_stealth_worker.txt"),
];
/// The `buildStealthInjectionScript` template; `__MAHO_HAS_SCRIPTS__` and `__MAHO_JOINT__` stand
/// for the template literal's two substitutions.
const INJECTION_TEMPLATE: &str = include_str!("../../assets/stealth/injection.js");

/// `isMicrosoftEdgeExecutable`.
pub fn is_microsoft_edge_executable(executable: Option<&str>) -> bool {
    let Some(executable) = executable.filter(|path| !path.is_empty()) else {
        return false;
    };
    let normalized = executable.replace('\\', "/").to_lowercase();
    let name = normalized.rsplit('/').next().unwrap_or_default();
    if name == "msedge.exe" {
        return true;
    }
    // `^microsoft[ -]edge(?:$|[ -])`
    ["microsoft edge", "microsoft-edge"].iter().any(|prefix| {
        name.strip_prefix(prefix)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '-']))
    })
}

/// `stealthIgnoreDefaultArgs`: puppeteer default switches a stealth launch leaves out. Edge keeps
/// `--enable-automation`, without which it can exit before DevTools opens.
pub fn stealth_ignore_default_args(executable: Option<&str>) -> Vec<&'static str> {
    STEALTH_IGNORE_DEFAULT_ARGS
        .into_iter()
        .filter(|arg| !is_microsoft_edge_executable(executable) || *arg != ENABLE_AUTOMATION_FLAG)
        .collect()
}

/// `buildStealthInjectionScript`.
pub fn build_stealth_injection_script(scripts: &[&str]) -> String {
    let joint = scripts
        .iter()
        .map(|script| format!("\n\t\ttry {{\n\t\t\t{script};\n\t\t}} catch (e) {{}}\n\t"))
        .collect::<Vec<_>>()
        .join(";\n");
    INJECTION_TEMPLATE
        .replace(
            "__MAHO_HAS_SCRIPTS__",
            if scripts.is_empty() { "false" } else { "true" },
        )
        .replace("__MAHO_JOINT__", &joint)
}

/// Host facts the user-agent metadata reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostInfo {
    /// `"arm"`, `"x86"` or `""` (`resolveHostArchitecture`).
    pub architecture: String,
    /// `"64"` or `""` (`resolveHostBitness`).
    pub bitness: String,
    /// macOS `ProductVersion`, empty elsewhere.
    pub macos_version: String,
}

impl HostInfo {
    pub fn current() -> Self {
        let arch = std::env::consts::ARCH;
        let architecture = if arch == "aarch64" || arch == "arm" {
            "arm"
        } else if arch.contains("64") {
            "x86"
        } else {
            ""
        };
        Self {
            architecture: architecture.into(),
            bitness: if arch.contains("64") { "64" } else { "" }.into(),
            macos_version: if cfg!(target_os = "macos") {
                std::fs::read_to_string("/System/Library/CoreServices/SystemVersion.plist")
                    .ok()
                    .and_then(|plist| {
                        let after = plist.split("<key>ProductVersion</key>").nth(1)?;
                        let start = after.find("<string>")? + "<string>".len();
                        let end = after[start..].find("</string>")?;
                        Some(after[start..start + end].to_owned())
                    })
                    .unwrap_or_default()
            } else {
                String::new()
            },
        }
    }
}

fn after<'a>(text: &'a str, marker: &str) -> Option<&'a str> {
    text.find(marker).map(|index| &text[index + marker.len()..])
}

fn version_digits(text: &str) -> Option<String> {
    let version: String = text
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '|')
        .collect();
    (!version.is_empty()).then_some(version)
}

/// `resolveUserAgentOverride`: the `Network/Emulation.setUserAgentOverride` params for a
/// browser reporting `raw_user_agent` and product `browser_version` (`Browser.getVersion`).
pub fn resolve_user_agent_override(
    raw_user_agent: &str,
    browser_version: &str,
    host: &HostInfo,
) -> Value {
    let mut user_agent = raw_user_agent.replacen("HeadlessChrome/", "Chrome/", 1);
    if user_agent.contains("Linux")
        && !user_agent.contains("Android")
        && let (Some(open), Some(close)) = (user_agent.find('('), user_agent.find(')'))
        && open < close
    {
        user_agent.replace_range(open..=close, "(Windows NT 10.0; Win64; x64)");
    }
    let ua_version = after(&user_agent, "Chrome/").and_then(version_digits);
    let browser_version = after(browser_version, "/").and_then(version_digits);
    let legacy = ua_version
        .clone()
        .or_else(|| browser_version.clone())
        .unwrap_or_else(|| "0".into());
    let full_version = browser_version.unwrap_or_else(|| legacy.clone());
    let major: usize = legacy
        .split('.')
        .next()
        .and_then(|major| major.parse().ok())
        .unwrap_or(0);
    let is_android = user_agent.contains("Android");
    let is_mac = user_agent.contains("Mac OS X");
    let is_windows = user_agent.contains("Windows");
    let is_linux = user_agent.contains("Linux");
    let platform = if is_mac {
        "MacIntel"
    } else if is_android {
        "Android"
    } else if is_linux {
        "Linux"
    } else {
        "Win32"
    };
    let platform_full = if is_mac {
        "macOS"
    } else if is_android {
        "Android"
    } else if is_linux {
        "Linux"
    } else {
        "Windows"
    };
    let platform_version = if is_mac {
        host.macos_version.clone()
    } else if user_agent.contains("Android ") {
        after(&user_agent, "Android ")
            .map(|rest| rest.split(';').next().unwrap_or_default().to_owned())
            .unwrap_or_default()
    } else if is_windows {
        after(&user_agent, "Windows NT ")
            .map(|rest| {
                rest.chars()
                    .take_while(|c| c.is_ascii_digit() || *c == '.')
                    .collect()
            })
            .unwrap_or_default()
    } else {
        String::new()
    };
    let model = if is_android {
        after(&user_agent, "Android")
            .and_then(|rest| {
                let semi = rest.find(';')?;
                let tail = rest[semi + 1..].trim_start();
                let end = tail.find(')')?;
                Some(tail[..end].to_owned())
            })
            .unwrap_or_default()
    } else {
        String::new()
    };
    const ORDERS: [[usize; 3]; 6] = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];
    let order = ORDERS[major % ORDERS.len()];
    let escaped = [" ", " ", ";"];
    let greasey = format!(
        "{}Not{}A{}Brand",
        escaped[order[0]], escaped[order[1]], escaped[order[2]]
    );
    let mut brands = vec![Value::Null; 3];
    brands[order[0]] = json!({"brand": greasey, "version": "99"});
    brands[order[1]] = json!({"brand": "Chromium", "version": major.to_string()});
    brands[order[2]] = json!({"brand": "Google Chrome", "version": major.to_string()});
    let full_version_list: Vec<Value> = brands
        .iter()
        .map(|brand| {
            let name = brand["brand"].as_str().unwrap_or_default();
            let version = if name == greasey {
                "99.0.0.0".to_owned()
            } else {
                full_version.clone()
            };
            json!({"brand": name, "version": version})
        })
        .collect();
    json!({
        "userAgent": user_agent,
        "platform": platform,
        "acceptLanguage": STEALTH_ACCEPT_LANGUAGE,
        "userAgentMetadata": {
            "brands": brands,
            "fullVersion": full_version,
            "fullVersionList": full_version_list,
            "platform": platform_full,
            "platformVersion": platform_version,
            "architecture": if is_android { String::new() } else { host.architecture.clone() },
            "bitness": if is_android { String::new() } else { host.bitness.clone() },
            "model": model,
            "mobile": is_android,
        },
    })
}

/// `targetInfoSupportsUserAgentOverride`.
pub fn target_supports_user_agent_override(target_type: Option<&str>) -> bool {
    target_type.is_some_and(|kind| USER_AGENT_TARGET_TYPES.contains(&kind))
}

/// `sendUserAgentOverride`: both overrides, each failure tolerated.
pub fn send_user_agent_override(client: &CdpClient, session: &str, override_params: &Value) {
    let _ = client.send("Network.enable", json!({}), Some(session));
    let _ = client.send(
        "Network.setUserAgentOverride",
        override_params.clone(),
        Some(session),
    );
    let _ = client.send(
        "Emulation.setUserAgentOverride",
        override_params.clone(),
        Some(session),
    );
}

/// Per-browser stealth state (`{ browserSession, override }`), so the override is resolved and
/// the browser-wide target listener started once.
#[derive(Debug, Default)]
pub struct StealthState {
    pub user_agent_override: Option<Value>,
    user_agent_targets: Option<UserAgentTargets>,
}

impl StealthState {
    /// Whether the browser-wide user-agent target listener is running.
    pub fn targets_configured(&self) -> bool {
        self.user_agent_targets.is_some()
    }
}

/// The auto-attach listener (oh-my-pi's `browserSession` and its `Target.attachedToTarget`
/// handler). Dropping it stops and joins the thread.
struct UserAgentTargets {
    cancel: SubscriptionCancel,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for UserAgentTargets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("UserAgentTargets")
    }
}

impl Drop for UserAgentTargets {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// `configureUserAgentTargets`: once per state, auto-attach browser-wide and override the user
/// agent of every page-like target that attaches later; then override every page-like target that
/// exists now through a temporary session.
pub fn configure_user_agent_targets(
    client: &CdpClient,
    state: &mut StealthState,
    override_params: &Value,
) -> Result<(), String> {
    if state.user_agent_targets.is_none() {
        let attached = client.subscribe(|event| {
            event.method == "Target.attachedToTarget" && event.session_id.is_none()
        });
        client.send(
            "Target.setAutoAttach",
            json!({"autoAttach": true, "waitForDebuggerOnStart": false, "flatten": true}),
            None,
        )?;
        let cancel = attached.canceller();
        let listener = client.clone().with_timeout(USER_AGENT_TARGET_TIMEOUT);
        let override_params = override_params.clone();
        let thread = std::thread::Builder::new()
            .name("stealth-targets".into())
            .spawn(move || {
                while attached.is_active() {
                    attached.wait(LISTENER_IDLE_WAIT);
                    for event in attached.drain() {
                        let kind = event.params.pointer("/targetInfo/type");
                        if !target_supports_user_agent_override(kind.and_then(Value::as_str)) {
                            continue;
                        }
                        if let Some(session) = event.params.get("sessionId").and_then(Value::as_str)
                        {
                            send_user_agent_override(&listener, session, &override_params);
                        }
                    }
                }
            })
            .map_err(|error| format!("cannot start the stealth target listener: {error}"))?;
        state.user_agent_targets = Some(UserAgentTargets {
            cancel,
            thread: Some(thread),
        });
    }
    let listed = client.send("Target.getTargets", json!({}), None)?;
    let targets = listed
        .get("targetInfos")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let bounded = client.clone().with_timeout(USER_AGENT_TARGET_TIMEOUT);
    for target in targets {
        if !target_supports_user_agent_override(target.get("type").and_then(Value::as_str)) {
            continue;
        }
        let Some(id) = target.get("targetId").and_then(Value::as_str) else {
            continue;
        };
        // `applyTargetUserAgentOverride`: a temporary session, detached afterwards.
        let Ok(attached) = bounded.send(
            "Target.attachToTarget",
            json!({"targetId": id, "flatten": true}),
            None,
        ) else {
            continue;
        };
        if let Some(session) = attached.get("sessionId").and_then(Value::as_str) {
            send_user_agent_override(&bounded, session, override_params);
            let _ = bounded.send(
                "Target.detachFromTarget",
                json!({"sessionId": session}),
                None,
            );
        }
    }
    Ok(())
}

/// `applyStealthPatches`: user-agent override on the page session, the browser-wide target
/// override ([`configure_user_agent_targets`]) and the patch scripts on every new document of the
/// page.
pub fn apply_stealth_patches(
    client: &CdpClient,
    session: &str,
    state: &mut StealthState,
) -> Result<(), String> {
    if state.user_agent_override.is_none() {
        let version = client.send("Browser.getVersion", json!({}), None)?;
        let text = |key: &str| {
            version
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        state.user_agent_override = Some(resolve_user_agent_override(
            &text("userAgent"),
            &text("product"),
            &HostInfo::current(),
        ));
    }
    let override_params = state.user_agent_override.clone().unwrap_or(Value::Null);
    send_user_agent_override(client, session, &override_params);
    configure_user_agent_targets(client, state, &override_params)?;
    client
        .send(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({"source": build_stealth_injection_script(&STEALTH_PATCH_SCRIPTS)}),
            Some(session),
        )
        .map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EDGE: [&str; 6] = [
        "C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "/Applications/Microsoft Edge Beta.app/Contents/MacOS/Microsoft Edge Beta",
        "/Applications/Microsoft Edge Dev.app/Contents/MacOS/Microsoft Edge Dev",
        "/Applications/Microsoft Edge Canary.app/Contents/MacOS/Microsoft Edge Canary",
        "/usr/bin/microsoft-edge-stable",
    ];
    const CHROME: [&str; 3] = [
        "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/usr/bin/chromium",
    ];

    #[test]
    fn keeps_puppeteers_automation_default_for_microsoft_edge_executables() {
        for path in EDGE {
            let args = stealth_ignore_default_args(Some(path));
            assert!(!args.contains(&ENABLE_AUTOMATION_FLAG), "{path}");
            assert!(args.contains(&"--disable-extensions"), "{path}");
        }
    }

    #[test]
    fn continues_filtering_puppeteers_automation_default_for_chrome_and_chromium_executables() {
        for path in CHROME {
            assert!(
                stealth_ignore_default_args(Some(path)).contains(&ENABLE_AUTOMATION_FLAG),
                "{path}"
            );
        }
        assert!(stealth_ignore_default_args(None).contains(&ENABLE_AUTOMATION_FLAG));
        assert!(!is_microsoft_edge_executable(Some(
            "/usr/bin/microsoft-edgeless"
        )));
    }

    #[test]
    fn the_injection_script_wraps_every_patch_and_proxies_to_string() {
        let script = build_stealth_injection_script(&STEALTH_PATCH_SCRIPTS);
        assert!(script.starts_with("(() => {"));
        assert!(script.trim_end().ends_with("}})();"));
        assert!(!script.contains("__MAHO_"));
        assert!(script.contains("if (true) {"));
        assert_eq!(script.matches("} catch (e) {}").count(), 14);
        for patch in STEALTH_PATCH_SCRIPTS {
            assert!(script.contains(patch.trim()));
        }
        let empty = build_stealth_injection_script(&[]);
        assert!(empty.contains("if (false) {"));
        assert!(!empty.contains("catch (e) {}"));
    }

    #[test]
    fn user_agent_override_hides_headless_linux_and_greases_brands() {
        let host = HostInfo {
            architecture: "x86".into(),
            bitness: "64".into(),
            macos_version: String::new(),
        };
        let ua = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) HeadlessChrome/150.0.7871.24 Safari/537.36";
        let result = resolve_user_agent_override(ua, "HeadlessChrome/150.0.7871.24", &host);
        assert_eq!(
            result["userAgent"],
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0.7871.24 Safari/537.36"
        );
        assert_eq!(result["platform"], "Win32");
        assert_eq!(result["acceptLanguage"], "en-US,en");
        let meta = &result["userAgentMetadata"];
        assert_eq!(meta["platform"], "Windows");
        assert_eq!(meta["platformVersion"], "10.0");
        assert_eq!(meta["fullVersion"], "150.0.7871.24");
        assert_eq!(meta["architecture"], "x86");
        assert_eq!(meta["bitness"], "64");
        assert_eq!(meta["mobile"], false);
        // 150 % 6 == 0: order [0, 1, 2].
        assert_eq!(
            meta["brands"],
            json!([
                {"brand": " Not A;Brand", "version": "99"},
                {"brand": "Chromium", "version": "150"},
                {"brand": "Google Chrome", "version": "150"}
            ])
        );
        assert_eq!(meta["fullVersionList"][0]["version"], "99.0.0.0");
        assert_eq!(meta["fullVersionList"][2]["version"], "150.0.7871.24");
        let mac = resolve_user_agent_override(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) Chrome/151.0.1.2 Safari/537.36",
            "Chrome/151.0.1.2",
            &HostInfo {
                macos_version: "15.1".into(),
                ..host
            },
        );
        assert_eq!(mac["platform"], "MacIntel");
        assert_eq!(mac["userAgentMetadata"]["platformVersion"], "15.1");
        // 151 % 6 == 1: order [0, 2, 1].
        assert_eq!(
            mac["userAgentMetadata"]["brands"][1]["brand"],
            "Google Chrome"
        );
    }

    #[test]
    fn user_agent_targets_are_pages_webviews_and_background_pages() {
        for kind in ["page", "webview", "background_page"] {
            assert!(target_supports_user_agent_override(Some(kind)));
        }
        assert!(!target_supports_user_agent_override(Some("service_worker")));
        assert!(!target_supports_user_agent_override(None));
    }

    #[test]
    fn user_agent_targets_cover_existing_and_later_attached_page_targets() {
        use crate::page::fake_cdp::FakeCdp;
        use std::sync::mpsc;
        let (overridden_tx, overridden) = mpsc::channel::<String>();
        let overridden_tx = std::sync::Mutex::new(overridden_tx);
        let cdp = FakeCdp::new(Box::new(
            move |method: &str, params: &Value, session: Option<&str>| {
                if method == "Emulation.setUserAgentOverride" {
                    let _ = overridden_tx
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .send(session.unwrap_or_default().to_owned());
                }
                Ok(match method {
                    "Target.getTargets" => json!({"targetInfos": [
                        {"targetId": "P", "type": "page"},
                        {"targetId": "W", "type": "service_worker"},
                    ]}),
                    "Target.attachToTarget" => {
                        json!({"sessionId": format!("tmp-{}", params["targetId"].as_str().unwrap_or_default())})
                    }
                    _ => json!({}),
                })
            },
        ));
        let mut state = StealthState::default();
        let ua = json!({"userAgent": "UA"});
        assert_eq!(
            configure_user_agent_targets(&cdp.client, &mut state, &ua),
            Ok(())
        );
        assert!(state.targets_configured());
        assert_eq!(
            overridden
                .recv_timeout(USER_AGENT_TARGET_TIMEOUT)
                .ok()
                .as_deref(),
            Some("tmp-P")
        );
        let auto = cdp.calls_of("Target.setAutoAttach");
        assert_eq!(auto.len(), 1);
        assert_eq!(
            auto[0].params,
            json!({"autoAttach": true, "waitForDebuggerOnStart": false, "flatten": true})
        );
        assert_eq!(
            cdp.calls_of("Target.attachToTarget").len(),
            1,
            "workers are skipped"
        );
        assert_eq!(
            cdp.calls_of("Target.detachFromTarget")[0].params["sessionId"],
            "tmp-P"
        );

        cdp.emit(
            "Target.attachedToTarget",
            json!({"sessionId": "worker", "targetInfo": {"type": "service_worker"}}),
            None,
        );
        cdp.emit(
            "Target.attachedToTarget",
            json!({"sessionId": "popup", "targetInfo": {"type": "page"}}),
            None,
        );
        assert_eq!(
            overridden
                .recv_timeout(USER_AGENT_TARGET_TIMEOUT)
                .ok()
                .as_deref(),
            Some("popup")
        );
        let popup = cdp
            .calls_of("Network.setUserAgentOverride")
            .into_iter()
            .find(|call| call.session.as_deref() == Some("popup"));
        assert_eq!(popup.map(|call| call.params), Some(ua.clone()));

        // A second configuration reuses the listener.
        assert_eq!(
            configure_user_agent_targets(&cdp.client, &mut state, &ua),
            Ok(())
        );
        assert_eq!(cdp.calls_of("Target.setAutoAttach").len(), 1);
        drop(state);
        assert!(
            cdp.calls()
                .iter()
                .all(|call| call.session.as_deref() != Some("worker"))
        );
    }
}
