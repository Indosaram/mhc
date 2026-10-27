//! A browser and its named tabs.
//!
//! Port of omowright `connectPipe`/`connect` + `BrowserConnection` (`src/pipe.js`,
//! `src/connection.js`: target listing, `newTab`, `attachPage`, `closeTab`, `close`) and of the
//! tab bookkeeping of oh-my-pi's browser tool (`tools/browser.ts`: named tabs with `main` as the
//! default, owned temporary profile removed after the browser exits, `close` of one tab or all).
use crate::cdp::CdpClient;
use crate::launch::{self, LaunchFeatures};
use crate::page::{NAVIGATION_TIMEOUT, Page, WaitUntil};
use crate::transport::{self, READINESS_TIMEOUT, StdioTail};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Child;

/// Tab name used when none is given (oh-my-pi `DEFAULT_TAB_NAME`).
pub const DEFAULT_TAB: &str = "main";

/// How the browser is reached.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TransportKind {
    /// `--remote-debugging-pipe` (Unix default).
    #[cfg_attr(unix, default)]
    Pipe,
    /// `--remote-debugging-port=0` + WebSocket (default where pipes are unavailable).
    #[cfg_attr(not(unix), default)]
    Port,
}

/// How to start a browser.
#[derive(Clone, Debug, PartialEq)]
pub struct LaunchOptions {
    /// Executable; found with [`launch::find_executable`] when `None`.
    pub executable: Option<PathBuf>,
    pub headless: bool,
    pub viewport: (u32, u32),
    /// Extra Chromium switches.
    pub args: Vec<String>,
    /// Profile directory; a temporary one (removed on close) when `None`.
    pub user_data_dir: Option<PathBuf>,
    pub features: LaunchFeatures,
    pub transport: TransportKind,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            executable: None,
            headless: true,
            viewport: launch::DEFAULT_VIEWPORT,
            args: Vec::new(),
            user_data_dir: None,
            features: LaunchFeatures::default(),
            transport: TransportKind::default(),
        }
    }
}

/// A page target as listed by the browser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetInfo {
    pub id: String,
    pub url: String,
    pub title: String,
    /// The tab name when this process opened it.
    pub name: Option<String>,
}

/// A connected browser.
pub struct Browser {
    client: CdpClient,
    child: Option<Child>,
    owned_profile: Option<PathBuf>,
    tail: StdioTail,
    viewport: Option<(u32, u32)>,
    tabs: BTreeMap<String, Page>,
    pub executable: Option<PathBuf>,
}

fn env_var(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

impl Browser {
    /// Launches a browser this process owns.
    pub fn launch(options: &LaunchOptions) -> Result<Self, String> {
        let executable = options
            .executable
            .clone()
            .or_else(|| launch::find_executable(&env_var))
            .ok_or_else(|| {
                format!(
                    "No Chrome/Chromium browser found. Install Chrome or Chromium, or set {} to a Chromium-based browser executable.",
                    launch::EXECUTABLE_ENV.join(" or ")
                )
            })?;
        let (profile, owned_profile) = match &options.user_data_dir {
            Some(dir) => (dir.clone(), None),
            None => {
                let dir = std::env::temp_dir().join(format!(
                    "maho-browser-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|elapsed| elapsed.as_nanos())
                        .unwrap_or_default()
                ));
                std::fs::create_dir_all(&dir)
                    .map_err(|error| format!("cannot create a browser profile directory: {error}"))?;
                (dir.clone(), Some(dir))
            }
        };
        let mut args = launch::launch_args(
            options.viewport,
            options.headless,
            &profile,
            &options.features,
            &env_var,
        );
        // Extra switches go before the start URL.
        let url = args.pop();
        args.extend(options.args.iter().cloned());
        args.extend(url);
        let launched = match options.transport {
            #[cfg(unix)]
            TransportKind::Pipe => transport::launch_pipe(&executable, &args, READINESS_TIMEOUT),
            #[cfg(not(unix))]
            TransportKind::Pipe => transport::launch_port(&executable, &args, READINESS_TIMEOUT),
            TransportKind::Port => transport::launch_port(&executable, &args, READINESS_TIMEOUT),
        };
        let launched = match launched {
            Ok(launched) => launched,
            Err(error) => {
                if let Some(dir) = &owned_profile {
                    let _ = std::fs::remove_dir_all(dir);
                }
                return Err(error);
            }
        };
        Ok(Self {
            client: launched.client,
            child: Some(launched.child),
            owned_profile,
            tail: launched.tail,
            viewport: Some(options.viewport),
            tabs: BTreeMap::new(),
            executable: Some(executable),
        })
    }

    /// Attaches to a browser someone else runs (`http://host:port` or `ws://...`).
    pub fn connect(endpoint: &str) -> Result<Self, String> {
        let client = transport::connect(endpoint)?;
        client.send("Browser.getVersion", json!({}), None)?;
        Ok(Self::from_client(client))
    }

    /// Wraps an already connected client (no process, no viewport override).
    pub fn from_client(client: CdpClient) -> Self {
        Self {
            client,
            child: None,
            owned_profile: None,
            tail: StdioTail::default(),
            viewport: None,
            tabs: BTreeMap::new(),
            executable: None,
        }
    }

    pub fn client(&self) -> &CdpClient {
        &self.client
    }

    /// Whether this process launched the browser.
    pub fn is_owned(&self) -> bool {
        self.child.is_some()
    }

    pub fn is_connected(&self) -> bool {
        self.client.is_connected()
    }

    /// The browser's recent stdout/stderr (launch diagnostics).
    pub fn stdio_tail(&self) -> String {
        self.tail.text()
    }

    /// Product and protocol version.
    pub fn version(&self) -> Result<String, String> {
        let version = self.client.send("Browser.getVersion", json!({}), None)?;
        Ok(version
            .get("product")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned())
    }

    /// The tab named `name`.
    pub fn tab(&mut self, name: &str) -> Option<&mut Page> {
        self.tabs.get_mut(name)
    }

    pub fn tab_names(&self) -> Vec<String> {
        self.tabs.keys().cloned().collect()
    }

    /// Opens tab `name` on a new target (or reuses it) and navigates it to `url` when given.
    /// Returns whether the navigation settled.
    pub fn open(&mut self, name: &str, url: Option<&str>, wait: WaitUntil) -> Result<bool, String> {
        if !self.tabs.contains_key(name) {
            let created = self.client.send(
                "Target.createTarget",
                json!({"url": "about:blank"}),
                None,
            )?;
            let target = created
                .get("targetId")
                .and_then(Value::as_str)
                .ok_or("Target.createTarget returned no targetId")?
                .to_owned();
            let page = Page::attach(self.client.clone(), &target)?;
            if let Some((width, height)) = self.viewport {
                page.set_viewport(width, height, 1.0)?;
            }
            self.tabs.insert(name.to_owned(), page);
        }
        match (url, self.tabs.get_mut(name)) {
            (Some(url), Some(page)) => page.goto(url, wait, NAVIGATION_TIMEOUT),
            _ => Ok(true),
        }
    }

    /// Page targets in the browser, with the names of the tabs this process opened.
    pub fn targets(&self) -> Result<Vec<TargetInfo>, String> {
        let listed = self.client.send("Target.getTargets", json!({}), None)?;
        Ok(listed
            .get("targetInfos")
            .and_then(Value::as_array)
            .map(|targets| {
                targets
                    .iter()
                    .filter(|target| target.get("type").and_then(Value::as_str) == Some("page"))
                    .map(|target| {
                        let id = target
                            .get("targetId")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                        let name = self
                            .tabs
                            .iter()
                            .find(|(_, page)| page.target_id == id)
                            .map(|(name, _)| name.clone());
                        TargetInfo {
                            url: target.get("url").and_then(Value::as_str).unwrap_or_default().to_owned(),
                            title: target.get("title").and_then(Value::as_str).unwrap_or_default().to_owned(),
                            id,
                            name,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Adopts an existing page target (e.g. a tab of a connected browser) as `name`.
    pub fn adopt(&mut self, name: &str, target_id: &str) -> Result<(), String> {
        let page = Page::attach(self.client.clone(), target_id)?;
        self.tabs.insert(name.to_owned(), page);
        Ok(())
    }

    /// Closes tab `name`. Returns whether it existed.
    pub fn close_tab(&mut self, name: &str) -> Result<bool, String> {
        let Some(page) = self.tabs.remove(name) else {
            return Ok(false);
        };
        if self.child.is_some() {
            self.client
                .send("Target.closeTarget", json!({"targetId": page.target_id}), None)?;
        } else {
            // A browser we do not own keeps its tabs; just detach.
            let _ = self.client.send(
                "Target.detachFromTarget",
                json!({"sessionId": page.session_id}),
                None,
            );
        }
        Ok(true)
    }

    /// Closes every tab and, when this process launched it, the browser.
    pub fn close(&mut self) {
        let names = self.tab_names();
        if self.child.is_none() {
            for name in names {
                let _ = self.close_tab(&name);
            }
        }
        self.tabs.clear();
        if let Some(mut child) = self.child.take() {
            transport::shutdown(&self.client, &mut child);
        }
        if let Some(dir) = self.owned_profile.take() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        self.close();
    }
}
