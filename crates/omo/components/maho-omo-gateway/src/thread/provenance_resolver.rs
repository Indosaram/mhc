//! Sender provenance classification.
//!
//! Upstream counterpart: `thread/gateway/provenance-resolver.ts`. Called in the sender runtime; a git
//! or config failure degrades the classification with a warning and never fails the send.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use super::types::Provenance;

/// One `provenance.workspaces` row.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenanceWorkspace {
    pub name: String,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub git_owners: Vec<String>,
}

/// One git run: its stdout, or how it failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitRun {
    Ok { stdout: String },
    Failed { code: Option<GitExitCode>, killed: bool, stderr: String, message: String },
}

/// A process exit code, or the OS error name (`ENOENT` for a missing binary).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitExitCode {
    Code(i32),
    Name(String),
}

impl GitExitCode {
    fn is_no_such_remote(&self) -> bool {
        matches!(self, Self::Code(2))
    }

    fn is_missing_binary(&self) -> bool {
        matches!(self, Self::Name(name) if name == "ENOENT")
    }
}

#[derive(Debug, Clone)]
pub struct GitOptions {
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub timeout_ms: u64,
}

pub const GIT_ORIGIN_TIMEOUT_MS: u64 = 1_000;

type ExecGit = Box<dyn Fn(&[&str], &GitOptions) -> GitRun>;
type ReadGitFile = Box<dyn Fn(&Path) -> String>;
type GitRemote = Box<dyn Fn(&Path) -> Option<String>>;
type LoadWorkspaces = Box<dyn Fn(&Path, &BTreeMap<String, String>) -> Vec<ProvenanceWorkspace>>;
type Warning = Box<dyn Fn(&str)>;

/// Everything `resolve_provenance` reads, with the runtime IO seams injected.
#[derive(Default)]
pub struct ProvenanceRuntime {
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub workspaces: Option<Vec<ProvenanceWorkspace>>,
    pub load_workspaces: Option<LoadWorkspaces>,
    pub on_warning: Option<Warning>,
    /// When given, replaces the whole git probe.
    pub git_remote: Option<GitRemote>,
    pub exec_git: Option<ExecGit>,
    pub read_git_file: Option<ReadGitFile>,
}

/// A `.git` directory, or a worktree `.git` file whose `gitdir:` target still exists.
fn live_git_entry(dir: &Path, read_git_file: &dyn Fn(&Path) -> String) -> bool {
    let entry = dir.join(".git");
    if !entry.exists() {
        return false;
    }
    if entry.is_dir() {
        return true;
    }
    let content = read_git_file(&entry);
    let target = content.lines().find_map(|line| line.trim().strip_prefix("gitdir:").map(|target| target.trim().to_owned()));
    target.is_some_and(|target| resolve_path(dir, Path::new(&target)).exists())
}

/// `resolve()`: a relative path resolves against `base`, `.`/`..` are folded lexically.
pub fn resolve_path(base: &Path, path: &Path) -> PathBuf {
    let joined = if path.is_absolute() { path.to_path_buf() } else { base.join(path) };
    let mut normalized = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    if normalized.as_os_str().is_empty() {
        PathBuf::from("/")
    } else {
        normalized
    }
}

fn env_paths(env: &BTreeMap<String, String>, key: &str) -> Vec<String> {
    env.get(key)
        .map(|value| value.split(':').filter(|path| Path::new(path).is_absolute()).map(|path| resolve_path(Path::new("/"), Path::new(path)).to_string_lossy().into_owned()).collect())
        .unwrap_or_default()
}

/// Structural, locale-independent: a repository is an explicit GIT_DIR or a live `.git` entry above
/// the cwd, and the walk stops below the sender's GIT_CEILING_DIRECTORIES.
fn inside_repository(cwd: &Path, env: &BTreeMap<String, String>, _warn: &dyn Fn(&str), read_git_file: &dyn Fn(&Path) -> String) -> bool {
    if env.get("GIT_DIR").is_some_and(|value| !value.is_empty()) {
        return true;
    }
    let ceilings = env_paths(env, "GIT_CEILING_DIRECTORIES");
    let mut dir = resolve_path(Path::new("/"), cwd);
    loop {
        if live_git_entry(&dir, read_git_file) {
            return true;
        }
        let Some(parent) = dir.parent().map(Path::to_path_buf) else {
            return false;
        };
        if parent == dir || ceilings.iter().any(|ceiling| Path::new(ceiling) == parent.as_path()) {
            return false;
        }
        dir = parent;
    }
}

/// The real `git` probe: one `git remote get-url origin` under the sender's own environment.
fn exec_git_file(args: &[&str], options: &GitOptions) -> GitRun {
    let mut command = Command::new("git");
    command.args(args).current_dir(&options.cwd).env_clear().envs(&options.env);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return GitRun::Failed { code: Some(GitExitCode::Name("ENOENT".into())), killed: false, stderr: String::new(), message: error.to_string() };
        }
        Err(error) => {
            return GitRun::Failed { code: None, killed: false, stderr: String::new(), message: error.to_string() };
        }
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(options.timeout_ms);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return GitRun::Failed { code: None, killed: true, stderr: String::new(), message: "git did not answer in time".into() };
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(5)),
            Err(error) => {
                return GitRun::Failed { code: None, killed: false, stderr: String::new(), message: error.to_string() };
            }
        }
    }
    match child.wait_with_output() {
        Ok(output) => GitRun::Ok { stdout: String::from_utf8_lossy(&output.stdout).into_owned() },
        Err(error) => GitRun::Failed { code: None, killed: false, stderr: String::new(), message: error.to_string() },
    }
}

fn read_file_default(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

fn no_warning(_: &str) {}

fn origin_remote(cwd: &Path, env: &BTreeMap<String, String>, warn: &dyn Fn(&str), runtime: &ProvenanceRuntime) -> Option<String> {
    let read_git_file: &dyn Fn(&Path) -> String = match &runtime.read_git_file {
        Some(read) => read.as_ref(),
        None => &read_file_default,
    };
    if !inside_repository(cwd, env, warn, read_git_file) {
        return None;
    }
    let options = GitOptions { cwd: cwd.to_path_buf(), env: env.clone(), timeout_ms: GIT_ORIGIN_TIMEOUT_MS };
    let run = match &runtime.exec_git {
        Some(exec) => exec(&["remote", "get-url", "origin"], &options),
        None => exec_git_file(&["remote", "get-url", "origin"], &options),
    };
    match run {
        GitRun::Ok { stdout } => Some(stdout.trim().to_owned()),
        GitRun::Failed { code, killed, stderr, message } => {
            match &code {
                Some(code) if code.is_missing_binary() => warn("git is not installed for the sender, so git does not classify this send"),
                _ if killed => warn(&format!("reading the sender's git origin failed, so git does not classify this send: git did not answer within {GIT_ORIGIN_TIMEOUT_MS} ms")),
                Some(code) if code.is_no_such_remote() => {}
                _ => {
                    let detail = if stderr.trim().is_empty() { message } else { stderr.trim().to_owned() };
                    warn(&format!("reading the sender's git origin failed, so git does not classify this send: {detail}"));
                }
            }
            None
        }
    }
}

/// The parsed repository of a remote: its project name and the candidate owners of a network remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRepository {
    pub owners: Vec<String>,
    pub project: String,
}

fn split_scheme(remote: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = remote.split_once("://")?;
    let valid = scheme.chars().next().is_some_and(|first| first.is_ascii_alphabetic())
        && scheme.chars().all(|character| character.is_ascii_alphanumeric() || matches!(character, '+' | '.' | '-'));
    valid.then_some((scheme, rest))
}

/// Parse only the path of a network remote: credentials and host identities never become labels.
pub fn repository(remote: Option<&str>) -> Option<RemoteRepository> {
    let remote = remote?;
    let mut owners_from_network = false;
    let path: String = if let Some((scheme, rest)) = split_scheme(remote) {
        // `<scheme>://[userinfo@]host[:port]/<path>`
        owners_from_network = !scheme.eq_ignore_ascii_case("file");
        rest.split_once('/').map(|(_, tail)| format!("/{tail}")).unwrap_or_default()
    } else if remote.len() >= 2 && remote.as_bytes()[1] == b':' && remote.as_bytes()[0].is_ascii_alphabetic() {
        remote.to_owned()
    } else if let Some((head, tail)) = remote.split_once(':') {
        // scp-like: `[user@]host:path`; a relative local path keeps its own shape.
        if head.contains('/') {
            remote.to_owned()
        } else {
            owners_from_network = true;
            tail.to_owned()
        }
    } else {
        remote.to_owned()
    };
    let trimmed = path.replace('\\', "/");
    let trimmed = trimmed.trim_end_matches('/');
    let parts: Vec<&str> = trimmed.split('/').filter(|part| !part.is_empty()).collect();
    let project = parts.last()?.trim_end_matches(".git").to_owned();
    if project.is_empty() {
        return None;
    }
    let owners = if owners_from_network { parts[..parts.len() - 1].iter().map(|owner| (*owner).to_owned()).collect() } else { Vec::new() };
    Some(RemoteRepository { owners, project })
}

fn expand_home(prefix: &str, home: &Path) -> Option<PathBuf> {
    if prefix == "~" {
        return Some(home.to_path_buf());
    }
    if let Some(rest) = prefix.strip_prefix("~/") {
        return Some(resolve_path(home, Path::new(rest)));
    }
    let path = Path::new(prefix);
    path.is_absolute().then(|| path.to_path_buf())
}

fn contains(cwd: &Path, prefix: &str, home: &Path) -> bool {
    let Some(expanded) = expand_home(prefix, home) else {
        return false;
    };
    let resolved_prefix = resolve_path(Path::new("/"), &expanded);
    let child = resolve_path(Path::new("/"), cwd).strip_prefix(&resolved_prefix).map(Path::to_path_buf);
    match child {
        Ok(child) => child.as_os_str().is_empty() || (!child.is_absolute() && child != Path::new("..") && !child.starts_with("..")),
        Err(_) => false,
    }
}

fn load_table(runtime: &ProvenanceRuntime, env: &BTreeMap<String, String>, _warn: &dyn Fn(&str)) -> Vec<ProvenanceWorkspace> {
    match &runtime.load_workspaces {
        Some(load) => load(&runtime.cwd, env),
        None => Vec::new(),
    }
}

fn trim_non_empty(value: Option<&String>) -> Option<String> {
    value.map(|value| value.trim().to_owned()).filter(|value| !value.is_empty())
}

/// Called in the sender runtime; the receiver renders only the provenance saved in the envelope.
pub fn resolve_provenance(runtime: &ProvenanceRuntime) -> Provenance {
    let env = if runtime.env.is_empty() { std::env::vars().collect() } else { runtime.env.clone() };
    let explicit_workspace = trim_non_empty(env.get("OMO_PROVENANCE_WORKSPACE"));
    let explicit_project = trim_non_empty(env.get("OMO_PROVENANCE_PROJECT"));
    let lane = trim_non_empty(env.get("OMO_PROVENANCE_LANE"));
    if let (Some(workspace), Some(project)) = (explicit_workspace.clone(), explicit_project.clone()) {
        return Provenance { workspace, project, lane, session: None };
    }
    let warn: &dyn Fn(&str) = match &runtime.on_warning {
        Some(warn) => warn.as_ref(),
        None => &no_warning,
    };
    let remote = match &runtime.git_remote {
        Some(git_remote) => git_remote(&runtime.cwd),
        None => origin_remote(&runtime.cwd, &env, warn, runtime),
    };
    let repo = repository(remote.as_deref());
    let project = explicit_project
        .clone()
        .or_else(|| repo.as_ref().map(|repo| repo.project.clone()))
        .or_else(|| resolve_path(Path::new("/"), &runtime.cwd).file_name().map(|name| name.to_string_lossy().into_owned()))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "root".to_owned());
    if let Some(workspace) = explicit_workspace {
        return Provenance { workspace, project, lane, session: None };
    }
    let table = match &runtime.workspaces {
        Some(workspaces) => workspaces.clone(),
        None => load_table(runtime, &env, warn),
    };
    let owners: Vec<String> = repo.as_ref().map(|repo| repo.owners.iter().map(|owner| owner.to_lowercase()).collect()).unwrap_or_default();
    let by_owner = (!owners.is_empty()).then(|| table.iter().find(|row| row.git_owners.iter().any(|candidate| owners.contains(&candidate.to_lowercase())))).flatten();
    let home = env
        .get("HOME")
        .or_else(|| env.get("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    let by_path = table.iter().find(|row| row.paths.iter().any(|prefix| contains(&runtime.cwd, prefix, &home)));
    let workspace = by_owner.or(by_path).map(|row| row.name.clone()).unwrap_or_else(|| "unclassified".to_owned());
    Provenance { workspace, project, lane, session: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A rooted path without a drive is not absolute on Windows, so the fixture root carries one there.
    #[cfg(windows)]
    const FIXTURE_ROOT: &str = "C:/tmp";
    #[cfg(not(windows))]
    const FIXTURE_ROOT: &str = "/tmp";

    fn workspace_table() -> Vec<ProvenanceWorkspace> {
        vec![
            ProvenanceWorkspace { name: "Path workspace".into(), paths: vec![FIXTURE_ROOT.into()], git_owners: vec![] },
            ProvenanceWorkspace { name: "Owner workspace".into(), paths: vec![], git_owners: vec!["example-owner".into()] },
        ]
    }

    fn runtime(remote: Option<&'static str>) -> ProvenanceRuntime {
        ProvenanceRuntime {
            cwd: PathBuf::from(format!("{FIXTURE_ROOT}/example-project")),
            env: BTreeMap::new(),
            workspaces: Some(workspace_table()),
            load_workspaces: None,
            on_warning: None,
            git_remote: Some(Box::new(move |_: &Path| remote.map(str::to_owned))),
            exec_git: None,
            read_git_file: None,
        }
    }

    #[test]
    fn explicit_workspace_project_and_lane_win_without_probing_git() {
        let mut probe = runtime(Some("git@example.invalid:example-owner/remote-project.git"));
        probe.git_remote = Some(Box::new(|_: &Path| panic!("git must not be read")));
        probe.env.insert("OMO_PROVENANCE_WORKSPACE".into(), "Explicit".into());
        probe.env.insert("OMO_PROVENANCE_PROJECT".into(), "explicit-project".into());
        probe.env.insert("OMO_PROVENANCE_LANE".into(), "review".into());
        assert_eq!(
            resolve_provenance(&probe),
            Provenance { workspace: "Explicit".into(), project: "explicit-project".into(), lane: Some("review".into()), session: None }
        );
    }

    #[test]
    fn matching_git_owner_wins_and_project_comes_from_the_remote() {
        let resolved = resolve_provenance(&runtime(Some("git@example.invalid:example-owner/remote-project.git")));
        assert_eq!(resolved.workspace, "Owner workspace");
        assert_eq!(resolved.project, "remote-project");
        assert_eq!(resolved.lane, None);
    }

    #[test]
    fn credentials_and_network_identity_never_become_labels() {
        for remote in [
            "git@fixture-machine.local:EXAMPLE-OWNER/remote-project.git",
            "https://user:password@192.0.2.7/EXAMPLE-OWNER/remote-project.git",
            "ssh://git@fixture-machine.local:2222/EXAMPLE-OWNER/remote-project.git",
        ] {
            let resolved = resolve_provenance(&runtime(Some(remote)));
            assert_eq!(resolved.workspace, "Owner workspace", "{remote}");
            assert_eq!(resolved.project, "remote-project", "{remote}");
        }
    }

    #[test]
    fn blank_overrides_fall_back_to_git_and_config() {
        let mut probe = runtime(Some("git@example.invalid:example-owner/remote-project.git"));
        probe.env.insert("OMO_PROVENANCE_WORKSPACE".into(), " ".into());
        probe.env.insert("OMO_PROVENANCE_PROJECT".into(), "\t".into());
        probe.env.insert("OMO_PROVENANCE_LANE".into(), String::new());
        assert_eq!(
            resolve_provenance(&probe),
            Provenance { workspace: "Owner workspace".into(), project: "remote-project".into(), lane: None, session: None }
        );
    }

    #[test]
    fn a_nonmatching_owner_falls_back_to_the_path_table() {
        let resolved = resolve_provenance(&runtime(Some("https://example.invalid/other/remote-project.git")));
        assert_eq!(resolved.workspace, "Path workspace");
        assert_eq!(resolved.project, "remote-project");
    }

    #[test]
    fn no_repository_stays_unclassified_with_the_cwd_basename() {
        let mut probe = runtime(None);
        probe.workspaces = Some(Vec::new());
        assert_eq!(resolve_provenance(&probe), Provenance { workspace: "unclassified".into(), project: "example-project".into(), lane: None, session: None });
    }

    #[test]
    fn individual_overrides_derive_the_rest_from_git_and_config() {
        let mut probe = runtime(Some("git@example.invalid:example-owner/remote-project.git"));
        probe.env.insert("OMO_PROVENANCE_WORKSPACE".into(), "Explicit".into());
        assert_eq!(resolve_provenance(&probe).project, "remote-project");
        let mut probe = runtime(Some("git@example.invalid:example-owner/remote-project.git"));
        probe.env.insert("OMO_PROVENANCE_PROJECT".into(), "explicit-project".into());
        let resolved = resolve_provenance(&probe);
        assert_eq!(resolved.workspace, "Owner workspace");
        assert_eq!(resolved.project, "explicit-project");
    }

    #[test]
    fn local_remotes_name_a_project_but_never_an_owner() {
        for remote in ["/example-owner/local-project.git", "file:///example-owner/local-project.git", "C:\\example-owner\\local-project.git"] {
            let resolved = resolve_provenance(&runtime(Some(remote)));
            assert_eq!(resolved.workspace, "Path workspace", "{remote}");
            assert_eq!(resolved.project, "local-project", "{remote}");
        }
    }

    #[test]
    fn tilde_prefixes_match_descendants_only() {
        let home = PathBuf::from("/tmp/example-home");
        let table = vec![ProvenanceWorkspace { name: "Path workspace".into(), paths: vec!["~/work".into()], git_owners: vec![] }];
        let probe = |cwd: &str| {
            let mut runtime = ProvenanceRuntime { cwd: PathBuf::from(cwd), env: BTreeMap::from([("HOME".to_owned(), home.to_string_lossy().into_owned())]), ..Default::default() };
            runtime.workspaces = Some(table.clone());
            runtime.git_remote = Some(Box::new(|_: &Path| None));
            resolve_provenance(&runtime)
        };
        assert_eq!(probe("/tmp/example-home/work/repo").workspace, "Path workspace");
        assert_eq!(probe("/tmp/example-home/work-other").workspace, "unclassified");
    }

    #[test]
    fn repository_parses_every_remote_form() {
        assert_eq!(repository(Some("git@example.invalid:owner/repo.git")), Some(RemoteRepository { owners: vec!["owner".into()], project: "repo".into() }));
        assert_eq!(repository(Some("https://host.example/owner/sub/repo.git")), Some(RemoteRepository { owners: vec!["owner".into(), "sub".into()], project: "repo".into() }));
        assert_eq!(repository(Some("/local/owner/repo")), Some(RemoteRepository { owners: vec![], project: "repo".into() }));
        assert_eq!(repository(Some("")), None);
        assert_eq!(repository(None), None);
    }
}
