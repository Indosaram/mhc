//! The on-disk store: live-session endpoint records and per-session inboxes (spec §2–§4).

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Environment variable naming the store root.
pub const DIR_ENV: &str = "MAHO_GATEWAY_DIR";
/// Format version written into every record.
pub const VERSION: u32 = 1;

/// The store root: `$MAHO_GATEWAY_DIR`, else `<agent dir>/gateway`.
pub fn default_root(agent_dir: &Path, env: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    match env(DIR_ENV).filter(|value| !value.trim().is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => agent_dir.join("gateway"),
    }
}

/// A live session that accepts deliveries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    pub version: u32,
    pub session_id: String,
    pub name: Option<String>,
    pub cwd: String,
    pub pid: u32,
    pub started_at: String,
}

/// Who sent a delivery.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Sender {
    Agent {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    CommandLine {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user: Option<String>,
    },
}

impl Sender {
    /// How the sender is named in the header the model reads.
    pub fn label(&self) -> String {
        match self {
            Self::Agent { session_id, name } => {
                let shown = name
                    .as_deref()
                    .filter(|name| !name.trim().is_empty())
                    .map_or_else(|| session_id.chars().take(8).collect(), str::to_owned);
                format!("session {shown}")
            }
            Self::CommandLine { user: Some(user) } if !user.trim().is_empty() => {
                format!("{user} (command line)")
            }
            Self::CommandLine { .. } => "command line".to_owned(),
        }
    }
}

/// Whether the receiver interrupts the current turn (`Steer`) or queues it (`FollowUp`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    #[serde(rename = "steer")]
    Steer,
    #[default]
    #[serde(rename = "followUp")]
    FollowUp,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Steer => "steer",
            Self::FollowUp => "followUp",
        }
    }
    /// `steer` / `followUp` (also `follow_up`, `follow-up`).
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "steer" => Some(Self::Steer),
            "followUp" | "follow_up" | "follow-up" | "followup" => Some(Self::FollowUp),
            _ => None,
        }
    }
}

/// One message waiting in a session's inbox.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delivery {
    pub version: u32,
    pub delivery_id: String,
    pub text: String,
    pub sender: Sender,
    pub deliver_as: Mode,
    pub hop: u32,
    pub created_at: String,
}

impl Delivery {
    /// A new delivery with a fresh id and the current time.
    pub fn new(text: impl Into<String>, sender: Sender, deliver_as: Mode, hop: u32) -> Self {
        Self {
            version: VERSION,
            delivery_id: uuid::Uuid::new_v4().simple().to_string(),
            text: text.into(),
            sender,
            deliver_as,
            hop: hop.max(1),
            created_at: now(),
        }
    }
}

/// The current time, RFC 3339 UTC with milliseconds (sorts lexicographically).
pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 200
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Writes `bytes` to `path` through a temp file in the same directory and a rename.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("path has no parent"))?;
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".tmp-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let result = (|| {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn json_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "json")
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| !name.starts_with('.'))
                && path.is_file()
        })
        .collect();
    files.sort();
    files
}

/// Whether `pid` is a running process (Linux: `/proc/<pid>`; elsewhere assumed).
pub fn process_alive(pid: u32) -> bool {
    if pid == std::process::id() {
        return true;
    }
    let proc_root = Path::new("/proc");
    if proc_root.join("self").exists() {
        proc_root.join(pid.to_string()).exists()
    } else {
        true
    }
}

/// Why a target did not resolve.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolveError {
    NotFound(String),
    Ambiguous(String, Vec<String>),
    SelfTarget,
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(target) => write!(f, "no live session matches \"{target}\""),
            Self::Ambiguous(target, candidates) => write!(
                f,
                "\"{target}\" matches several live sessions: {}",
                candidates.join(", ")
            ),
            Self::SelfTarget => write!(f, "a session cannot send a message to itself"),
        }
    }
}

/// The gateway store rooted at a directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }
    fn endpoint_path(&self, session_id: &str) -> PathBuf {
        self.sessions_dir().join(format!("{session_id}.json"))
    }
    /// The inbox directory of `session_id`.
    pub fn inbox_dir(&self, session_id: &str) -> PathBuf {
        self.root.join("inbox").join(session_id)
    }

    /// Writes `endpoint`'s record and creates its inbox (0700 on Unix).
    ///
    /// # Errors
    /// Invalid session id or a filesystem error.
    pub fn register(&self, endpoint: &Endpoint) -> std::io::Result<()> {
        if !valid_id(&endpoint.session_id) {
            return Err(std::io::Error::other(format!(
                "invalid session id {:?}",
                endpoint.session_id
            )));
        }
        let inbox = self.inbox_dir(&endpoint.session_id);
        fs::create_dir_all(&inbox)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&inbox, fs::Permissions::from_mode(0o700));
        }
        let bytes = serde_json::to_vec_pretty(endpoint).map_err(std::io::Error::other)?;
        write_atomic(&self.endpoint_path(&endpoint.session_id), &bytes)
    }

    /// Removes the endpoint record of `session_id` (its inbox stays for pending deliveries).
    pub fn unregister(&self, session_id: &str) {
        if valid_id(session_id) {
            let _ = fs::remove_file(self.endpoint_path(session_id));
        }
    }

    /// Every parseable endpoint record, live or not.
    pub fn endpoints(&self) -> Vec<Endpoint> {
        json_files(&self.sessions_dir())
            .into_iter()
            .filter_map(|path| serde_json::from_slice(&fs::read(path).ok()?).ok())
            .collect()
    }

    /// Live endpoints, oldest first; records of dead processes are removed.
    pub fn live(&self) -> Vec<Endpoint> {
        let mut live: Vec<Endpoint> = self
            .endpoints()
            .into_iter()
            .filter(|endpoint| {
                let alive = process_alive(endpoint.pid);
                if !alive {
                    self.unregister(&endpoint.session_id);
                }
                alive
            })
            .collect();
        live.sort_by(|a, b| {
            a.started_at
                .cmp(&b.started_at)
                .then(a.session_id.cmp(&b.session_id))
        });
        live
    }

    /// Resolves `target` (exact id, case-insensitive name, unique id prefix of ≥ 4 chars) among
    /// live sessions; `caller` is refused as a target.
    ///
    /// # Errors
    /// No match, several matches, or the caller itself.
    pub fn resolve(&self, target: &str, caller: Option<&str>) -> Result<Endpoint, ResolveError> {
        let target = target.trim();
        let live = self.live();
        let pick = |matches: Vec<&Endpoint>| -> Option<Result<Endpoint, ResolveError>> {
            match matches.as_slice() {
                [] => None,
                [one] => Some(if Some(one.session_id.as_str()) == caller {
                    Err(ResolveError::SelfTarget)
                } else {
                    Ok((*one).clone())
                }),
                many => Some(Err(ResolveError::Ambiguous(
                    target.to_owned(),
                    many.iter().map(|e| e.session_id.clone()).collect(),
                ))),
            }
        };
        if target.is_empty() {
            return Err(ResolveError::NotFound(target.to_owned()));
        }
        if let Some(result) = pick(live.iter().filter(|e| e.session_id == target).collect()) {
            return result;
        }
        let lower = target.to_lowercase();
        if let Some(result) = pick(
            live.iter()
                .filter(|e| e.name.as_deref().is_some_and(|n| n.to_lowercase() == lower))
                .collect(),
        ) {
            return result;
        }
        if target.chars().count() >= 4
            && let Some(result) = pick(
                live.iter()
                    .filter(|e| e.session_id.starts_with(target))
                    .collect(),
            )
        {
            return result;
        }
        Err(ResolveError::NotFound(target.to_owned()))
    }

    /// Writes `delivery` into `session_id`'s inbox.
    ///
    /// # Errors
    /// Invalid ids or a filesystem error.
    pub fn deliver(&self, session_id: &str, delivery: &Delivery) -> std::io::Result<PathBuf> {
        if !valid_id(session_id) || !valid_id(&delivery.delivery_id) {
            return Err(std::io::Error::other("invalid session or delivery id"));
        }
        let path = self
            .inbox_dir(session_id)
            .join(format!("{}.json", delivery.delivery_id));
        let bytes = serde_json::to_vec_pretty(delivery).map_err(std::io::Error::other)?;
        write_atomic(&path, &bytes)?;
        Ok(path)
    }

    /// Pending deliveries of `session_id`, oldest `created_at` first, with their files.
    pub fn pending(&self, session_id: &str) -> Vec<(PathBuf, Delivery)> {
        if !valid_id(session_id) {
            return Vec::new();
        }
        let mut pending: Vec<(PathBuf, Delivery)> = json_files(&self.inbox_dir(session_id))
            .into_iter()
            .filter_map(|path| {
                let delivery: Delivery = serde_json::from_slice(&fs::read(&path).ok()?).ok()?;
                valid_id(&delivery.delivery_id).then_some((path, delivery))
            })
            .collect();
        pending.sort_by(|a, b| {
            a.1.created_at
                .cmp(&b.1.created_at)
                .then(a.1.delivery_id.cmp(&b.1.delivery_id))
        });
        pending
    }
}
