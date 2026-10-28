//! Server table: project and user `lsp-client.json` overrides merged over the builtin servers.
use crate::servers::{BUILTIN_SERVERS, INSTALL_HINTS};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A server ready to spawn.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedServer {
    pub id: String,
    pub command: Vec<String>,
    pub extensions: Vec<String>,
    pub priority: i64,
    pub env: BTreeMap<String, String>,
    pub initialization: Option<Value>,
    pub request_timeout: Option<Duration>,
    pub init_timeout: Option<Duration>,
}

impl ResolvedServer {
    pub fn new(id: &str, command: &[&str], extensions: &[&str]) -> Self {
        Self {
            id: id.to_owned(),
            command: command.iter().map(|part| (*part).to_owned()).collect(),
            extensions: extensions.iter().map(|part| (*part).to_owned()).collect(),
            priority: 0,
            env: BTreeMap::new(),
            initialization: None,
            request_timeout: None,
            init_timeout: None,
        }
    }
}

/// Where a merged server came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ServerSource {
    Project,
    User,
    Builtin,
}

impl ServerSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::User => "user",
            Self::Builtin => "builtin",
        }
    }
}

/// Config files, highest precedence first within each scope. Each file holds
/// `{"lsp": {"<id>": {"command": [...], "extensions": [...], "priority", "env", "initialization",
/// "requestTimeoutMs", "initTimeoutMs", "disabled"}}}`.
#[derive(Clone, Debug, Default)]
pub struct ConfigPaths {
    pub project: Vec<PathBuf>,
    pub user: Vec<PathBuf>,
}

impl ConfigPaths {
    /// `<cwd>/.omo/lsp-client.json`, `<cwd>/.pi/lsp-client.json`; `<agent_dir>/lsp-client.json`,
    /// `~/.pi/lsp-client.json`.
    pub fn standard(cwd: &Path, agent_dir: Option<&Path>, home: Option<&Path>) -> Self {
        let mut user = Vec::new();
        if let Some(agent_dir) = agent_dir {
            user.push(agent_dir.join("lsp-client.json"));
        }
        if let Some(home) = home {
            user.push(home.join(".pi").join("lsp-client.json"));
        }
        Self {
            project: vec![
                cwd.join(".omo").join("lsp-client.json"),
                cwd.join(".pi").join("lsp-client.json"),
            ],
            user,
        }
    }
}

/// A merged server and its origin.
#[derive(Clone, Debug, PartialEq)]
pub struct MergedServer {
    pub server: ResolvedServer,
    pub source: ServerSource,
}

fn load_entries(paths: &[PathBuf]) -> Vec<Map<String, Value>> {
    paths
        .iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .filter_map(|text| serde_json::from_str::<Value>(&text).ok())
        .filter_map(|value| value.get("lsp").and_then(Value::as_object).cloned())
        .collect()
}

fn strings(value: Option<&Value>) -> Option<Vec<String>> {
    value?
        .as_array()?
        .iter()
        .map(|item| item.as_str().map(str::to_owned))
        .collect()
}

fn positive_ms(value: Option<&Value>) -> Option<Duration> {
    let ms = value?.as_f64()?;
    (ms.is_finite() && ms > 0.0).then(|| Duration::from_millis(ms as u64))
}

/// Merged servers: project entries, then user entries, then builtins; within a source by
/// descending priority. A `disabled` entry hides the id everywhere below it.
pub fn merged_servers(paths: &ConfigPaths) -> Vec<MergedServer> {
    let mut servers = Vec::new();
    let mut disabled = HashSet::new();
    let mut seen = HashSet::new();
    for (source, files) in [
        (ServerSource::Project, &paths.project),
        (ServerSource::User, &paths.user),
    ] {
        for entries in load_entries(files) {
            for (id, entry) in entries {
                if entry.get("disabled").and_then(Value::as_bool) == Some(true) {
                    disabled.insert(id);
                    continue;
                }
                if seen.contains(&id) {
                    continue;
                }
                let (Some(command), Some(extensions)) = (
                    strings(entry.get("command")),
                    strings(entry.get("extensions")),
                ) else {
                    continue;
                };
                let env = entry
                    .get("env")
                    .and_then(Value::as_object)
                    .map(|env| {
                        env.iter()
                            .filter_map(|(key, value)| {
                                Some((key.clone(), value.as_str()?.to_owned()))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                servers.push(MergedServer {
                    server: ResolvedServer {
                        id: id.clone(),
                        command,
                        extensions,
                        priority: entry.get("priority").and_then(Value::as_i64).unwrap_or(0),
                        env,
                        initialization: entry.get("initialization").cloned(),
                        request_timeout: positive_ms(entry.get("requestTimeoutMs")),
                        init_timeout: positive_ms(entry.get("initTimeoutMs")),
                    },
                    source,
                });
                seen.insert(id);
            }
        }
    }
    for (id, command, extensions) in BUILTIN_SERVERS {
        if disabled.contains(*id) || seen.contains(*id) {
            continue;
        }
        let mut server = ResolvedServer::new(id, command, extensions);
        server.priority = -100;
        servers.push(MergedServer {
            server,
            source: ServerSource::Builtin,
        });
    }
    servers.sort_by(|a, b| {
        a.source
            .cmp(&b.source)
            .then(b.server.priority.cmp(&a.server.priority))
    });
    servers
}

/// Ids disabled by any config file.
pub fn disabled_server_ids(paths: &ConfigPaths) -> Vec<String> {
    let mut ids: Vec<String> = load_entries(&paths.project)
        .into_iter()
        .chain(load_entries(&paths.user))
        .flat_map(|entries| {
            entries
                .into_iter()
                .filter(|(_, entry)| entry.get("disabled").and_then(Value::as_bool) == Some(true))
                .map(|(id, _)| id)
        })
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

pub fn install_hint(id: &str) -> Option<&'static str> {
    INSTALL_HINTS
        .iter()
        .find(|(key, _)| *key == id)
        .map(|(_, hint)| *hint)
}

/// Outcome of looking up a server for a file extension.
#[derive(Clone, Debug, PartialEq)]
pub enum ServerLookup {
    Found(ResolvedServer),
    NotConfigured {
        extension: String,
        available: Vec<String>,
    },
    NotInstalled {
        server: ResolvedServer,
        install_hint: String,
    },
}

/// First installed server for `extension`, else the first configured one (not installed), else
/// "not configured".
pub fn find_server_for_extension(paths: &ConfigPaths, extension: &str, cwd: &Path) -> ServerLookup {
    let servers = merged_servers(paths);
    if let Some(found) = servers.iter().find(|merged| {
        merged.server.extensions.iter().any(|ext| ext == extension)
            && is_server_installed(&merged.server.command, cwd)
    }) {
        return ServerLookup::Found(found.server.clone());
    }
    if let Some(configured) = servers
        .iter()
        .find(|merged| merged.server.extensions.iter().any(|ext| ext == extension))
    {
        let first = configured
            .server
            .command
            .first()
            .cloned()
            .unwrap_or_default();
        let install_hint = install_hint(&configured.server.id)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Install '{first}' and ensure it's in your PATH"));
        return ServerLookup::NotInstalled {
            server: configured.server.clone(),
            install_hint,
        };
    }
    let mut available = Vec::new();
    for merged in &servers {
        if !available.contains(&merged.server.id) {
            available.push(merged.server.id.clone());
        }
    }
    ServerLookup::NotConfigured {
        extension: extension.to_owned(),
        available,
    }
}

/// Lookup failure text.
pub fn format_lookup_error(lookup: &ServerLookup) -> String {
    match lookup {
        ServerLookup::Found(_) => String::new(),
        ServerLookup::NotInstalled {
            server,
            install_hint,
        } => [
            format!(
                "LSP server '{}' is configured but NOT INSTALLED.",
                server.id
            ),
            String::new(),
            format!(
                "Command not found: {}",
                server
                    .command
                    .first()
                    .map(String::as_str)
                    .unwrap_or_default()
            ),
            String::new(),
            "To install:".to_owned(),
            format!("  {install_hint}"),
            String::new(),
            format!("Supported extensions: {}", server.extensions.join(", ")),
            String::new(),
            "After installation, the server will be available automatically.".to_owned(),
        ]
        .join("\n"),
        ServerLookup::NotConfigured {
            extension,
            available,
        } => {
            let more = if available.len() > 10 { "..." } else { "" };
            let shown: Vec<&str> = available.iter().take(10).map(String::as_str).collect();
            [
                format!("No LSP server configured for extension: {extension}"),
                String::new(),
                format!("Available servers: {}{more}", shown.join(", ")),
                String::new(),
                "Configure a custom server in '.omo/lsp-client.json':".to_owned(),
                "  {".to_owned(),
                "    \"lsp\": {".to_owned(),
                "      \"my-server\": {".to_owned(),
                "        \"command\": [\"my-lsp\", \"--stdio\"],".to_owned(),
                format!("        \"extensions\": [\"{extension}\"]"),
                "      }".to_owned(),
                "    }".to_owned(),
                "  }".to_owned(),
            ]
            .join("\n")
        }
    }
}

/// `<root>/node_modules/.bin`, searched after `PATH`.
pub fn additional_path_bases(root: &Path) -> Vec<PathBuf> {
    vec![root.join("node_modules").join(".bin")]
}

/// Whether `command[0]` resolves: an existing path, a `PATH` entry, or `<cwd>/node_modules/.bin`.
pub fn is_server_installed(command: &[String], cwd: &Path) -> bool {
    let Some(program) = command.first().filter(|program| !program.is_empty()) else {
        return false;
    };
    if (program.contains('/') || program.contains('\\')) && Path::new(program).exists() {
        return true;
    }
    let suffixes: Vec<String> = if cfg!(windows) {
        let mut list = vec![String::new()];
        let path_ext = std::env::var("PATHEXT").unwrap_or_default();
        for ext in path_ext
            .split(';')
            .filter(|ext| !ext.is_empty())
            .map(str::to_owned)
            .chain([".exe", ".cmd", ".bat", ".ps1"].map(str::to_owned))
        {
            if !list.contains(&ext) {
                list.push(ext);
            }
        }
        list
    } else {
        vec![String::new()]
    };
    let path = std::env::var_os("PATH")
        .or_else(|| std::env::var_os("Path"))
        .unwrap_or_default();
    let directories = std::env::split_paths(&path).chain(additional_path_bases(cwd));
    for directory in directories {
        for suffix in &suffixes {
            if directory.join(format!("{program}{suffix}")).is_file() {
                return true;
            }
        }
    }
    program == "node"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_overrides_user_and_builtin_and_disables() {
        let dir = tempfile::tempdir().expect("tempdir");
        let project = dir.path().join("project.json");
        let user = dir.path().join("user.json");
        std::fs::write(&project, r#"{"lsp":{"mine":{"command":["my-lsp"],"extensions":[".foo"],"initTimeoutMs":5,"requestTimeoutMs":-1},"rust":{"disabled":true}}}"#).expect("write");
        std::fs::write(&user, r#"{"lsp":{"mine":{"command":["other"],"extensions":[".foo"]},"u":{"command":["u"],"extensions":[".u"],"priority":3,"env":{"A":"1"}}}}"#).expect("write");
        let paths = ConfigPaths {
            project: vec![project],
            user: vec![user],
        };
        let merged = merged_servers(&paths);
        assert_eq!(merged[0].server.id, "mine");
        assert_eq!(merged[0].server.command, vec!["my-lsp"]);
        assert_eq!(
            merged[0].server.init_timeout,
            Some(Duration::from_millis(5))
        );
        assert_eq!(merged[0].server.request_timeout, None);
        assert_eq!(merged[1].source, ServerSource::User);
        assert_eq!(merged[1].server.env.get("A").map(String::as_str), Some("1"));
        assert!(merged.iter().all(|server| server.server.id != "rust"));
        assert_eq!(
            merged
                .iter()
                .filter(|server| server.server.id == "mine")
                .count(),
            1
        );
        assert_eq!(disabled_server_ids(&paths), vec!["rust"]);
    }

    #[test]
    fn lookup_reports_not_installed_and_not_configured() {
        let paths = ConfigPaths::default();
        let cwd = Path::new("/nonexistent-root");
        match find_server_for_extension(&paths, ".definitely-not-a-language", cwd) {
            ServerLookup::NotConfigured {
                extension,
                available,
            } => {
                assert_eq!(extension, ".definitely-not-a-language");
                assert!(available.contains(&"typescript".to_owned()));
            }
            other => panic!("unexpected {other:?}"),
        }
        let lookup = ServerLookup::NotInstalled {
            server: ResolvedServer::new("gopls", &["gopls"], &[".go"]),
            install_hint: "go install".into(),
        };
        let text = format_lookup_error(&lookup);
        assert!(text.starts_with("LSP server 'gopls' is configured but NOT INSTALLED."));
        assert!(text.contains("Command not found: gopls"));
    }

    #[test]
    fn installed_checks_path_and_node_modules_bin() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin = dir.path().join("node_modules/.bin");
        std::fs::create_dir_all(&bin).expect("mkdir");
        std::fs::write(bin.join("local-lsp"), "").expect("write");
        assert!(is_server_installed(&["local-lsp".into()], dir.path()));
        assert!(!is_server_installed(
            &["surely-missing-lsp-binary".into()],
            dir.path()
        ));
        assert!(!is_server_installed(&[], dir.path()));
        assert!(is_server_installed(&["node".into()], dir.path()));
    }
}
