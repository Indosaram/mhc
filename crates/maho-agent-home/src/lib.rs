//! The per-user agent home of mhc's MIT profile (spec `docs/cleanroom/specs/agent-home.md`):
//! where the user configuration, the agent dir and its subdirectories live, a bootstrap that
//! creates the missing directories, a privacy check of `auth.json`, and the first-run setup state.

pub mod cli;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Kind of a layout entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Dir,
    File,
}

/// One named path of the layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: &'static str,
    pub path: PathBuf,
    pub kind: Kind,
    /// The file holds secrets and must be private (Unix mode without group/other bits).
    pub secret: bool,
}

/// The layout of one agent home.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentHome {
    root: PathBuf,
    agent: PathBuf,
}

/// The state file of first-run setup (`<root>/state/setup.json`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupState {
    pub version: u32,
    pub completed_at: String,
    pub provider: Option<String>,
    pub model: Option<String>,
}

/// What bootstrap did to one directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bootstrapped {
    Created,
    Present,
}

/// Inspection result of one entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inspected {
    pub entry: Entry,
    pub exists: bool,
    pub problem: Option<String>,
}

/// Name of the legacy completion marker honoured by [`AgentHome::setup_version`].
pub const LEGACY_MARKER: &str = "onboarding-completed";

impl AgentHome {
    /// The layout for `home` (the user's home directory) and the already-resolved agent dir.
    pub fn new(home: &Path, agent_dir: &Path) -> Self {
        Self {
            root: home.join(".omo"),
            agent: agent_dir.to_path_buf(),
        }
    }

    /// `<home>/.omo`.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The agent dir.
    pub fn agent_dir(&self) -> &Path {
        &self.agent
    }

    /// The user configuration file: `omo.jsonc`, else an existing `omo.json`.
    pub fn config_file(&self) -> PathBuf {
        let jsonc = self.root.join("omo.jsonc");
        let json = self.root.join("omo.json");
        if !jsonc.exists() && json.exists() {
            json
        } else {
            jsonc
        }
    }

    pub fn auth_file(&self) -> PathBuf {
        self.agent.join("auth.json")
    }

    pub fn models_file(&self) -> PathBuf {
        self.agent.join("models.json")
    }

    pub fn settings_file(&self) -> PathBuf {
        self.agent.join("settings.json")
    }

    /// `<root>/state`: per-feature state of MIT crates.
    pub fn state_dir(&self) -> PathBuf {
        self.root.join("state")
    }

    /// `<root>/state/setup.json`.
    pub fn setup_file(&self) -> PathBuf {
        self.state_dir().join("setup.json")
    }

    /// Every entry of the layout, in the spec's order.
    pub fn entries(&self) -> Vec<Entry> {
        let entry = |name, path, kind, secret| Entry {
            name,
            path,
            kind,
            secret,
        };
        vec![
            entry("root", self.root.clone(), Kind::Dir, false),
            entry("config", self.config_file(), Kind::File, false),
            entry("agent", self.agent.clone(), Kind::Dir, false),
            entry("auth", self.auth_file(), Kind::File, true),
            entry("models", self.models_file(), Kind::File, false),
            entry("settings", self.settings_file(), Kind::File, false),
            entry("sessions", self.agent.join("sessions"), Kind::Dir, false),
            entry("skills", self.agent.join("skills"), Kind::Dir, false),
            entry(
                "extensions",
                self.agent.join("extensions"),
                Kind::Dir,
                false,
            ),
            entry("prompts", self.agent.join("prompts"), Kind::Dir, false),
            entry("bin", self.agent.join("bin"), Kind::Dir, false),
            entry("state", self.state_dir(), Kind::Dir, false),
            entry("setup", self.setup_file(), Kind::File, false),
        ]
    }

    /// Creates every missing directory of the layout (mode 0700 on Unix); never touches files.
    pub fn bootstrap(&self) -> std::io::Result<Vec<(Entry, Bootstrapped)>> {
        let mut out = Vec::new();
        for entry in self.entries().into_iter().filter(|e| e.kind == Kind::Dir) {
            if entry.path.is_dir() {
                out.push((entry, Bootstrapped::Present));
                continue;
            }
            create_private_dir(&entry.path)?;
            out.push((entry, Bootstrapped::Created));
        }
        Ok(out)
    }

    /// Existence and problems of every entry.
    pub fn inspect(&self) -> Vec<Inspected> {
        self.entries()
            .into_iter()
            .map(|entry| {
                let meta = std::fs::metadata(&entry.path).ok();
                let exists = meta.is_some();
                let problem = meta.and_then(|meta| {
                    let wrong = match entry.kind {
                        Kind::Dir => !meta.is_dir(),
                        Kind::File => meta.is_dir(),
                    };
                    if wrong {
                        return Some("wrong kind".to_owned());
                    }
                    if entry.secret {
                        return not_private(&meta);
                    }
                    None
                });
                Inspected {
                    entry,
                    exists,
                    problem,
                }
            })
            .collect()
    }

    /// Bootstraps, then makes a non-private `auth.json` private. Returns the repaired paths.
    pub fn repair(&self) -> std::io::Result<Vec<PathBuf>> {
        self.bootstrap()?;
        let mut repaired = Vec::new();
        for inspected in self.inspect() {
            if inspected.entry.secret
                && inspected
                    .problem
                    .as_deref()
                    .is_some_and(|p| p.starts_with("secret file"))
            {
                make_private(&inspected.entry.path)?;
                repaired.push(inspected.entry.path);
            }
        }
        Ok(repaired)
    }

    /// The stored setup state, when `setup.json` parses.
    pub fn setup_state(&self) -> Option<SetupState> {
        let text = std::fs::read_to_string(self.setup_file()).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// The stored setup version: `setup.json`, else 1 for a legacy marker, else 0.
    pub fn setup_version(&self) -> u32 {
        if let Some(state) = self.setup_state() {
            return state.version;
        }
        let legacy = [
            self.root.join(LEGACY_MARKER),
            self.agent
                .join("omo-senpi")
                .join("omo-native")
                .join(LEGACY_MARKER),
        ];
        u32::from(legacy.iter().any(|path| path.is_file()))
    }

    /// Records a finished setup.
    pub fn mark_setup_complete(
        &self,
        version: u32,
        provider: Option<&str>,
        model: Option<&str>,
    ) -> std::io::Result<SetupState> {
        let state = SetupState {
            version,
            completed_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            provider: provider.map(str::to_owned),
            model: model.map(str::to_owned),
        };
        let dir = self.state_dir();
        if !dir.is_dir() {
            create_private_dir(&dir)?;
        }
        let text = serde_json::to_string_pretty(&state).map_err(std::io::Error::other)?;
        write_atomic(&self.setup_file(), format!("{text}\n").as_bytes())?;
        Ok(state)
    }

    /// The `mhc home --json` document.
    pub fn report_json(&self) -> Value {
        let entries: Vec<Value> = self
            .inspect()
            .into_iter()
            .map(|i| {
                json!({
                    "name": i.entry.name,
                    "path": i.entry.path.to_string_lossy(),
                    "kind": i.entry.kind,
                    "exists": i.exists,
                    "problem": i.problem,
                })
            })
            .collect();
        json!({
            "root": self.root.to_string_lossy(),
            "entries": entries,
            "setup_version": self.setup_version(),
        })
    }
}

fn create_private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path)
    }
}

#[cfg(unix)]
fn not_private(meta: &std::fs::Metadata) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    let mode = meta.permissions().mode() & 0o777;
    (meta.is_file() && mode & 0o077 != 0)
        .then(|| format!("secret file is not private (mode {mode:03o})"))
}

#[cfg(not(unix))]
fn not_private(_meta: &std::fs::Metadata) -> Option<String> {
    None
}

#[cfg(unix)]
fn make_private(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn make_private(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Writes `bytes` to a temporary file beside `path`, then renames it into place.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = dir.join(format!(".tmp-{}-{name}", std::process::id()));
    std::fs::write(&temp, bytes)?;
    std::fs::rename(&temp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> (tempfile::TempDir, AgentHome) {
        let dir = tempfile::tempdir().expect("temp");
        let home = AgentHome::new(dir.path(), &dir.path().join(".omo/agent"));
        (dir, home)
    }

    #[test]
    fn bootstrap_creates_every_dir_once_and_no_file() {
        let (_dir, home) = layout();
        let first = home.bootstrap().expect("bootstrap");
        assert!(first.iter().all(|(_, b)| *b == Bootstrapped::Created));
        assert_eq!(first.len(), 8);
        for entry in home.entries() {
            match entry.kind {
                Kind::Dir => assert!(entry.path.is_dir(), "{}", entry.name),
                Kind::File => assert!(!entry.path.exists(), "{}", entry.name),
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(home.state_dir())
                .expect("state")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        let second = home.bootstrap().expect("again");
        assert!(second.iter().all(|(_, b)| *b == Bootstrapped::Present));
    }

    #[test]
    fn existing_directories_keep_their_mode() {
        let (_dir, home) = layout();
        std::fs::create_dir_all(home.root()).expect("root");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(home.root(), std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
            home.bootstrap().expect("bootstrap");
            let mode = std::fs::metadata(home.root())
                .expect("root")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o755);
        }
    }

    #[cfg(unix)]
    #[test]
    fn group_readable_auth_is_flagged_and_repaired() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, home) = layout();
        home.bootstrap().expect("bootstrap");
        std::fs::write(home.auth_file(), "{}").expect("auth");
        std::fs::set_permissions(home.auth_file(), std::fs::Permissions::from_mode(0o644))
            .expect("chmod");
        let auth = home
            .inspect()
            .into_iter()
            .find(|i| i.entry.name == "auth")
            .expect("auth entry");
        assert_eq!(
            auth.problem.as_deref(),
            Some("secret file is not private (mode 644)")
        );
        assert_eq!(home.repair().expect("repair"), vec![home.auth_file()]);
        assert!(home.inspect().iter().all(|i| i.problem.is_none()));
    }

    #[test]
    fn wrong_kinds_are_reported() {
        let (_dir, home) = layout();
        std::fs::create_dir_all(home.agent_dir()).expect("agent");
        std::fs::write(home.agent_dir().join("sessions"), "").expect("file where a dir belongs");
        std::fs::create_dir_all(home.models_file()).expect("dir where a file belongs");
        let problems: Vec<_> = home
            .inspect()
            .into_iter()
            .filter_map(|i| i.problem.map(|p| (i.entry.name, p)))
            .collect();
        assert_eq!(
            problems,
            vec![
                ("models", "wrong kind".to_owned()),
                ("sessions", "wrong kind".to_owned())
            ]
        );
    }

    #[test]
    fn setup_version_prefers_state_then_legacy_markers() {
        let (_dir, home) = layout();
        assert_eq!(home.setup_version(), 0);
        std::fs::create_dir_all(home.agent_dir().join("omo-senpi/omo-native")).expect("native");
        std::fs::write(
            home.agent_dir()
                .join("omo-senpi/omo-native")
                .join(LEGACY_MARKER),
            "{}\n",
        )
        .expect("marker");
        assert_eq!(home.setup_version(), 1);
        let state = home
            .mark_setup_complete(3, Some("offline"), Some("offline/m"))
            .expect("mark");
        assert_eq!(home.setup_version(), 3);
        assert_eq!(home.setup_state(), Some(state));
    }

    #[test]
    fn root_marker_counts_as_completed() {
        let (_dir, home) = layout();
        std::fs::create_dir_all(home.root()).expect("root");
        std::fs::write(home.root().join(LEGACY_MARKER), "").expect("marker");
        assert_eq!(home.setup_version(), 1);
    }

    #[test]
    fn config_file_prefers_jsonc_unless_only_json_exists() {
        let (_dir, home) = layout();
        assert!(home.config_file().ends_with("omo.jsonc"));
        std::fs::create_dir_all(home.root()).expect("root");
        std::fs::write(home.root().join("omo.json"), "{}").expect("json");
        assert!(home.config_file().ends_with("omo.json"));
        std::fs::write(home.root().join("omo.jsonc"), "{}").expect("jsonc");
        assert!(home.config_file().ends_with("omo.jsonc"));
    }

    #[test]
    fn report_json_has_the_documented_shape() {
        let (_dir, home) = layout();
        let report = home.report_json();
        assert_eq!(report["setup_version"], 0);
        let entries = report["entries"].as_array().expect("entries");
        assert_eq!(entries.len(), 13);
        assert_eq!(entries[0]["name"], "root");
        assert_eq!(entries[0]["kind"], "dir");
        assert_eq!(entries[0]["exists"], false);
        assert!(entries[0]["problem"].is_null());
    }
}
