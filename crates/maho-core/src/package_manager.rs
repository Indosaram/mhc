//! Port of senpi `packages/coding-agent/src/core/package-manager.ts` restricted to resource
//! packages (skills, prompts, themes); code extensions are native Rust crates (plan D-M5), so the
//! extension-package branches of the senpi file have no counterpart here.

use crate::source_info::SourceScope;
use crate::source_info::SourceOrigin;
use crate::settings_manager::{Settings, SettingsManager, SettingsScope};
use crate::skill_discovery::{SkillDiscoveryMode, collect_skill_entries, collect_auto_skill_entries};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum PackageManagerError {
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type PackageResult<T> = Result<T, PackageManagerError>;
pub type ProgressCallback = Arc<dyn Fn(ProgressEvent) + Send + Sync>;
pub type MissingSourceCallback = dyn Fn(&str) -> futures::future::BoxFuture<'_, MissingSourceAction> + Send + Sync;

/// Commands are argv, never shell fragments. Capture commands have a bounded lifetime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageCommand {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub capture: bool,
    pub timeout_ms: Option<u64>,
    pub env: std::collections::HashMap<String, String>,
}

/// Both async operations and legacy global-root probes use this injectable seam.
#[async_trait::async_trait]
pub trait PackageProcessRunner: Send + Sync {
    async fn run(&self, command: PackageCommand) -> PackageResult<String>;
    fn run_sync(&self, command: PackageCommand) -> PackageResult<String>;
}

#[derive(Debug, Default)]
pub struct SystemPackageProcessRunner;

fn command_output(command: &PackageCommand, output: std::process::Output) -> PackageResult<String> {
    if !output.status.success() {
        return Err(PackageManagerError::Message(format!("{} {} failed with {}: {}", command.program,
            command.args.join(" "), output.status, String::from_utf8_lossy(&output.stderr))));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

#[async_trait::async_trait]
impl PackageProcessRunner for SystemPackageProcessRunner {
    async fn run(&self, command: PackageCommand) -> PackageResult<String> {
        let mut child = tokio::process::Command::new(&command.program);
        child.args(&command.args).envs(&command.env).kill_on_drop(true);
        if let Some(cwd) = &command.cwd { child.current_dir(cwd); }
        child.stdin(std::process::Stdio::null());
        if command.capture {
            child.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
        } else {
            child.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::inherit());
        }
        let operation = child.output();
        let output = if let Some(ms) = command.timeout_ms {
            tokio::time::timeout(std::time::Duration::from_millis(ms), operation).await
                .map_err(|_| PackageManagerError::Message(format!("{} {} timed out after {ms}ms", command.program, command.args.join(" "))))??
        } else { operation.await? };
        if !command.capture { crate::output_guard::maho_write_stdout(&String::from_utf8_lossy(&output.stdout)); }
        command_output(&command, output)
    }

    fn run_sync(&self, command: PackageCommand) -> PackageResult<String> {
        let mut child = std::process::Command::new(&command.program);
        child.args(&command.args).envs(&command.env).stdin(std::process::Stdio::null());
        if let Some(cwd) = &command.cwd { child.current_dir(cwd); }
        command_output(&command, child.output()?)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PackageOptions { pub local: bool }

#[derive(Debug, Clone, Copy, Default)]
pub struct ResolveSourcesOptions { pub local: bool, pub temporary: bool }

/// Borrow the caller's manager; persistence updates that exact instance, not a snapshot.
pub struct PackageManagerOptions<'a, 'paths> {
    pub cwd: &'paths str,
    pub agent_dir: &'paths str,
    pub settings_manager: &'a mut SettingsManager,
}

#[derive(Debug, Clone)]
enum ParsedSource {
    Npm { spec: String, name: String, version: Option<String> },
    Git { repo: String, host: String, path: String, reference: Option<String> },
    Local(String),
}

fn parse_source(source: &str) -> ParsedSource {
    if let Some(spec) = source.strip_prefix("npm:") {
        let spec = spec.trim();
        let version_offset = spec.rfind('@').filter(|offset| *offset > 0 && offset + 1 < spec.len());
        let (name, version) = match version_offset {
            Some(offset) => (&spec[..offset], Some(spec[offset + 1..].to_owned())),
            None => (spec, None),
        };
        return ParsedSource::Npm { spec: spec.to_owned(), name: name.to_owned(), version };
    }
    if crate::paths::is_local_path(source) { return ParsedSource::Local(source.to_owned()); }
    if let Some(parsed) = parse_git_source(source) { return parsed; }
    ParsedSource::Local(source.to_owned())
}

fn parse_git_source(source: &str) -> Option<ParsedSource> {
    let trimmed = source.trim();
    let prefixed = trimmed.starts_with("git:");
    let url = if prefixed { trimmed.strip_prefix("git:")?.trim() } else { trimmed };
    if !prefixed && !["http://", "https://", "ssh://", "git://"].iter().any(|prefix| url.starts_with(prefix)) { return None; }
    let path_start = if let Some(rest) = url.strip_prefix("git@") {
        rest.find(':')? + 5
    } else if let Some((scheme, rest)) = url.split_once("://") {
        scheme.len() + 3 + rest.find('/')? + 1
    } else { url.find('/')? + 1 };
    let (repo, reference) = if let Some(offset) = url[path_start..].find(['@', '#']) {
        let offset = path_start + offset;
        (&url[..offset], Some(url[offset + 1..].to_owned()).filter(|value| !value.is_empty()))
    } else { (url, None) };
    let (host, path) = if let Some(rest) = repo.strip_prefix("git@") {
        rest.split_once(':')?
    } else if let Some((_, rest)) = repo.split_once("://") {
        let (authority, path) = rest.split_once('/')?;
        (authority.rsplit('@').next()?.split(':').next()?, path)
    } else { repo.split_once('/')? };
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    if host.is_empty() || (!host.contains('.') && host != "localhost") || path.split('/').count() < 2 { return None; }
    for part in [host, path] {
        // Decode percent escapes before checking managed-storage traversal.
        let mut decoded = Vec::new();
        let mut bytes = part.bytes();
        while let Some(byte) = bytes.next() {
            if byte == b'%' {
                let high = char::from(bytes.next()?).to_digit(16)?;
                let low = char::from(bytes.next()?).to_digit(16)?;
                decoded.push(u8::try_from(high * 16 + low).ok()?);
            } else { decoded.push(byte); }
        }
        let decoded = String::from_utf8(decoded).ok()?;
        if decoded.contains(['\0', '\\']) || decoded.starts_with('/') || decoded.split('/').any(|part| part == "..") { return None; }
    }
    let repo = if repo.contains("://") || repo.starts_with("git@") { repo.to_owned() } else { format!("https://{repo}") };
    Some(ParsedSource::Git { repo, host: host.to_owned(), path: path.to_owned(), reference })
}

fn join_path(root: &str, part: &str) -> String { Path::new(root).join(part).to_string_lossy().into_owned() }

fn relative_path(base: &str, path: &str) -> String {
    let base: Vec<_> = Path::new(base).components().collect();
    let path: Vec<_> = Path::new(path).components().collect();
    let common = base.iter().zip(&path).take_while(|(left, right)| left == right).count();
    let mut result = PathBuf::new();
    for _ in &base[common..] { result.push(".."); }
    for part in &path[common..] { result.push(part.as_os_str()); }
    result.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/")
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value.and_then(Value::as_array).map(|values| values.iter().filter_map(Value::as_str).map(str::to_owned).collect()).unwrap_or_default()
}

fn source_string(pkg: &Value) -> Option<&str> { pkg.as_str().or_else(|| pkg.get("source").and_then(Value::as_str)) }

/// Resource-only manager. Native extension registration is owned by maho-ext-host;
/// this manager discovers data, including hook JSON, but never loads JS/TS code.
pub struct DefaultPackageManager<'a> {
    cwd: String,
    agent_dir: String,
    settings_manager: &'a mut SettingsManager,
    runner: Arc<dyn PackageProcessRunner>,
    progress_callback: Option<ProgressCallback>,
    global_npm_root: std::sync::Mutex<Option<(Vec<String>, String)>>,
    home_dir: String,
    env: std::collections::HashMap<String, String>,
}

impl<'a> DefaultPackageManager<'a> {
    pub fn new(options: PackageManagerOptions<'a, '_>) -> Self {
        Self::with_runner(options, Arc::new(SystemPackageProcessRunner))
    }

    pub fn with_runner(options: PackageManagerOptions<'a, '_>, runner: Arc<dyn PackageProcessRunner>) -> Self {
        let home_dir = crate::config::home_dir();
        let base = std::env::current_dir().map(|path| path.to_string_lossy().into_owned()).unwrap_or_else(|_| ".".to_owned());
        Self {
            cwd: crate::paths::resolve_path(options.cwd, &base, &crate::paths::PathInputOptions { trim: true, ..Default::default() }),
            agent_dir: crate::paths::resolve_path(options.agent_dir, &base, &crate::paths::PathInputOptions { trim: true, ..Default::default() }),
            settings_manager: options.settings_manager, runner, progress_callback: None,
            global_npm_root: std::sync::Mutex::new(None), home_dir, env: std::env::vars().collect(),
        }
    }

    pub fn set_progress_callback(&mut self, callback: Option<ProgressCallback>) { self.progress_callback = callback; }

    fn emit(&self, event_type: ProgressEventType, action: ProgressAction, source: &str, message: Option<String>) {
        if let Some(callback) = &self.progress_callback { callback(ProgressEvent { event_type, action, source: source.to_owned(), message }); }
    }

    fn finish_progress<T>(&self, action: ProgressAction, source: &str, result: PackageResult<T>) -> PackageResult<T> {
        match &result {
            Ok(_) => self.emit(ProgressEventType::Complete, action, source, None),
            Err(error) => self.emit(ProgressEventType::Error, action, source, Some(error.to_string())),
        }
        result
    }

    fn trusted(&self, scope: SourceScope) -> PackageResult<()> {
        if scope == SourceScope::Project && !self.settings_manager.is_project_trusted() {
            return Err(PackageManagerError::Message("Project is not trusted; refusing to access project package storage".to_owned()));
        }
        Ok(())
    }

    fn scope(options: Option<PackageOptions>) -> SourceScope {
        if options.is_some_and(|options| options.local) { SourceScope::Project } else { SourceScope::User }
    }

    fn base_dir(&self, scope: SourceScope) -> PackageResult<String> {
        self.trusted(scope)?;
        Ok(match scope {
            SourceScope::Project => join_path(&self.cwd, &crate::config::config_dir_name()),
            SourceScope::User => self.agent_dir.clone(),
            SourceScope::Temporary | SourceScope::System => self.cwd.clone(),
        })
    }

    fn resolve_path(&self, source: &str, base: &str) -> String {
        crate::paths::resolve_path(source, base, &crate::paths::PathInputOptions {
            trim: true, home_dir: Some(self.home_dir.clone()), ..Default::default()
        })
    }

    fn identity(&self, source: &str, scope: Option<SourceScope>) -> PackageResult<String> {
        Ok(match parse_source(source) {
            ParsedSource::Npm { name, .. } => format!("npm:{name}"),
            ParsedSource::Git { host, path, .. } => format!("git:{host}/{path}"),
            ParsedSource::Local(path) => format!("local:{}", self.resolve_path(&path, &match scope {
                Some(scope) => self.base_dir(scope)?, None => self.cwd.clone(),
            })),
        })
    }

    fn packages(&self, scope: SourceScope) -> Vec<Value> {
        let settings = if scope == SourceScope::Project { self.settings_manager.get_project() } else { self.settings_manager.get_global() };
        settings.get("packages").and_then(Value::as_array).cloned().unwrap_or_default()
    }

    pub fn add_source_to_settings(&mut self, source: &str, options: Option<PackageOptions>) -> PackageResult<bool> {
        let scope = Self::scope(options);
        let input_key = self.identity(source, None)?;
        let normalized = match parse_source(source) {
            ParsedSource::Local(path) => {
                let relative = relative_path(&self.base_dir(scope)?, &self.resolve_path(&path, &self.cwd));
                if relative.is_empty() { ".".to_owned() } else { relative }
            }
            ParsedSource::Npm { .. } | ParsedSource::Git { .. } => source.to_owned(),
        };
        let mut packages = self.packages(scope);
        let mut matched = None;
        for (index, package) in packages.iter().enumerate() {
            if let Some(existing) = source_string(package) && self.identity(existing, Some(scope))? == input_key { matched = Some(index); break; }
        }
        if let Some(index) = matched {
            if source_string(&packages[index]) == Some(normalized.as_str()) { return Ok(false); }
            if let Some(object) = packages[index].as_object_mut() { object.insert("source".to_owned(), Value::String(normalized)); }
            else { packages[index] = Value::String(normalized); }
        } else { packages.push(Value::String(normalized)); }
        self.persist_packages(scope, packages)?;
        Ok(true)
    }

    pub fn remove_source_from_settings(&mut self, source: &str, options: Option<PackageOptions>) -> PackageResult<bool> {
        let scope = Self::scope(options);
        let identity = self.identity(source, None)?;
        let packages = self.packages(scope);
        let mut next = Vec::new();
        for package in &packages {
            if let Some(existing) = source_string(package) && self.identity(existing, Some(scope))? == identity { continue; }
            next.push(package.clone());
        }
        if next.len() == packages.len() { return Ok(false); }
        self.persist_packages(scope, next)?;
        Ok(true)
    }

    fn persist_packages(&mut self, scope: SourceScope, packages: Vec<Value>) -> PackageResult<()> {
        self.trusted(scope)?;
        let mut update = Settings::new();
        update.insert("packages".to_owned(), Value::Array(packages));
        self.settings_manager.set(if scope == SourceScope::Project { SettingsScope::Project } else { SettingsScope::Global }, &update)
            .map_err(PackageManagerError::Message)
    }

    fn command(&self, program: &str, args: Vec<String>, cwd: Option<&str>, capture: bool) -> PackageCommand {
        let mut env = self.env.clone();
        if program == "git" && args.first().is_some_and(|arg| arg == "ls-remote") {
            env.insert("GIT_TERMINAL_PROMPT".into(), "0".into());
            env.insert("GIT_SSH_COMMAND".into(), "ssh -o BatchMode=yes -o ConnectTimeout=10".into());
        }
        PackageCommand { program: program.to_owned(), args, cwd: cwd.map(str::to_owned), capture,
            timeout_ms: capture.then_some(10_000), env }
    }

    async fn run(&self, program: &str, args: Vec<String>, cwd: Option<&str>, capture: bool) -> PackageResult<String> {
        self.runner.run(self.command(program, args, cwd, capture)).await
    }

    fn npm_command(&self) -> PackageResult<Vec<String>> {
        let configured = strings(self.settings_manager.get_value("npmCommand"));
        if configured.is_empty() { return Ok(vec!["npm".to_owned()]); }
        if configured.first().is_some_and(String::is_empty) {
            return Err(PackageManagerError::Message("Invalid npmCommand: first array entry must be a non-empty command".to_owned()));
        }
        Ok(configured)
    }

    fn npm_name(command: &[String]) -> String {
        let command = command.iter().rposition(|part| part == "--").and_then(|index| command.get(index + 1)).or_else(|| command.first());
        let name = command.and_then(|command| Path::new(command).file_name()).map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        name.strip_suffix(".cmd").or_else(|| name.strip_suffix(".exe")).unwrap_or(&name).to_owned()
    }

    async fn npm(&self, args: Vec<String>, cwd: Option<&str>, capture: bool) -> PackageResult<String> {
        let command = self.npm_command()?;
        let (program, prefix) = command.split_first().ok_or_else(|| PackageManagerError::Message("Missing npm command".to_owned()))?;
        let mut all_args = prefix.to_vec(); all_args.extend(args);
        self.run(program, all_args, cwd, capture).await
    }

    fn npm_sync(&self, args: Vec<String>) -> PackageResult<String> {
        let command = self.npm_command()?;
        let (program, prefix) = command.split_first().ok_or_else(|| PackageManagerError::Message("Missing npm command".to_owned()))?;
        let mut all_args = prefix.to_vec(); all_args.extend(args);
        self.runner.run_sync(self.command(program, all_args, None, true))
    }

    fn managed_path(&self, root: &str, part: &str) -> PackageResult<String> {
        let path = crate::paths::lexical_resolve(&join_path(root, part));
        if !Path::new(&path).starts_with(root) {
            return Err(PackageManagerError::Message(format!("Refusing to use path outside package install root: {path}")));
        }
        Ok(path)
    }

    fn temporary_dir(&self, prefix: &str, suffix: &str) -> PackageResult<String> {
        let root = join_path(&self.agent_dir, "tmp/extensions");
        std::fs::create_dir_all(&root)?;
        #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?; }
        let hash = hex::encode(Sha256::digest(format!("{prefix}-{suffix}")));
        self.managed_path(&root, &format!("{prefix}/{}/{suffix}", &hash[..8]))
    }

    fn install_root(&self, kind: &str, scope: SourceScope) -> PackageResult<String> {
        self.trusted(scope)?;
        if scope == SourceScope::Temporary { self.temporary_dir(kind, "") }
        else { Ok(join_path(&self.base_dir(scope)?, kind)) }
    }

    fn managed_install_path(&self, source: &ParsedSource, scope: SourceScope) -> PackageResult<String> {
        match source {
            ParsedSource::Npm { name, .. } => self.managed_path(&self.install_root("npm", scope)?, &format!("node_modules/{name}")),
            ParsedSource::Git { host, path, .. } => if scope == SourceScope::Temporary {
                self.temporary_dir(&format!("git-{host}"), path)
            } else { self.managed_path(&self.install_root("git", scope)?, &format!("{host}/{path}")) },
            ParsedSource::Local(path) => Ok(self.resolve_path(path, &self.base_dir(scope)?)),
        }
    }

    fn installed_path(&self, parsed: &ParsedSource, scope: SourceScope) -> PackageResult<String> {
        let managed = self.managed_install_path(parsed, scope)?;
        if let ParsedSource::Npm { name, .. } = parsed && scope == SourceScope::User && !Path::new(&managed).exists() {
            let legacy = (|| -> PackageResult<String> {
                let command = self.npm_command()?;
                let manager = Self::npm_name(&command);
                if manager == "pnpm" {
                    let output = self.npm_sync(vec!["list".into(), "-g".into(), "--depth".into(), "0".into(), "--json".into()])?;
                    if let Ok(Value::Array(entries)) = serde_json::from_str::<Value>(&output) {
                        for entry in entries { if let Some(path) = entry.get("dependencies").and_then(|deps| deps.get(name)).and_then(|dep| dep.get("path")).and_then(Value::as_str) { return Ok(path.to_owned()); } }
                    }
                }
                let mut cache = self.global_npm_root.lock().map_err(|error| PackageManagerError::Message(error.to_string()))?;
                if let Some((key, root)) = &*cache && *key == command { return Ok(join_path(root, name)); }
                let root = if manager == "bun" {
                    let bin = self.npm_sync(vec!["pm".into(), "bin".into(), "-g".into()])?;
                    join_path(&parent_path(&bin), "install/global/node_modules")
                } else { self.npm_sync(vec!["root".into(), "-g".into()])? };
                let path = join_path(&root, name);
                *cache = Some((command, root));
                Ok(path)
            })();
            if let Ok(legacy) = legacy && Path::new(&legacy).exists() { return Ok(legacy); }
        }
        Ok(managed)
    }

    pub fn get_installed_path(&self, source: &str, scope: InstalledSourceScope) -> PackageResult<Option<String>> {
        let scope = match scope { InstalledSourceScope::User => SourceScope::User, InstalledSourceScope::Project => SourceScope::Project };
        let path = self.installed_path(&parse_source(source), scope)?;
        Ok(Path::new(&path).exists().then_some(path))
    }

    pub fn list_configured_packages(&self) -> PackageResult<Vec<ConfiguredPackage>> {
        let mut configured = Vec::new();
        for (scope, installed_scope) in [(SourceScope::User, InstalledSourceScope::User), (SourceScope::Project, InstalledSourceScope::Project)] {
            for pkg in self.packages(scope) {
                if let Some(source) = source_string(&pkg) { configured.push(ConfiguredPackage { source: source.to_owned(), scope: installed_scope,
                    filtered: pkg.is_object(), installed_path: self.get_installed_path(source, installed_scope)? }); }
            }
        }
        Ok(configured)
    }

    fn ensure_npm_project(&self, root: &str) -> PackageResult<()> {
        ensure_ignore(root)?;
        let attributes: &[&str] = if cfg!(target_os = "macos") { &["com.dropbox.ignored", "com.apple.fileprovider.ignore#P"] }
            else if cfg!(target_os = "linux") { &["user.com.dropbox.ignored"] } else { &[] };
        for attribute in attributes {
            let (program, args) = if cfg!(target_os = "macos") { ("xattr", vec!["-w".into(), (*attribute).into(), "1".into(), root.into()]) }
                else { ("setfattr", vec!["-n".into(), (*attribute).into(), "-v".into(), "1".into(), root.into()]) };
            if let Err(error) = self.runner.run_sync(self.command(program, args, None, true)) { eprintln!("Could not mark package storage ignored by cloud sync: {error}"); }
        }
        let manifest = join_path(root, "package.json");
        if !Path::new(&manifest).exists() { std::fs::write(manifest, "{\n  \"name\": \"pi-extensions\",\n  \"private\": true\n}")?; }
        Ok(())
    }

    fn npm_install_args(&self, specs: Vec<String>, root: &str) -> PackageResult<Vec<String>> {
        let manager = Self::npm_name(&self.npm_command()?);
        let mut args = vec!["install".to_owned()]; args.extend(specs);
        if manager == "bun" { args.extend(["--cwd".into(), root.into(), "--omit=peer".into()]); }
        else {
            args.extend(["--prefix".into(), root.into()]);
            if manager == "pnpm" { args.extend(["--config.auto-install-peers=false".into(), "--config.strict-peer-dependencies=false".into(), "--config.strict-dep-builds=false".into()]); }
            else { args.push("--legacy-peer-deps".into()); }
        }
        Ok(args)
    }

    async fn install_npm_specs(&self, specs: Vec<String>, scope: SourceScope) -> PackageResult<()> {
        let root = self.install_root("npm", scope)?;
        self.ensure_npm_project(&root)?;
        self.npm(self.npm_install_args(specs, &root)?, None, false).await?;
        Ok(())
    }

    async fn git_dependencies(&self, target: &str) -> PackageResult<()> {
        if Path::new(target).join("package.json").exists() {
            let mut args = vec!["install".into()];
            if strings(self.settings_manager.get_value("npmCommand")).is_empty() { args.push("--omit=dev".into()); }
            self.npm(args, Some(target), false).await?;
        }
        Ok(())
    }

    async fn repair_git_dependencies(&self, target: &str) -> PackageResult<()> {
        let manifest = read_json(&join_path(target, "package.json"));
        if let Some(dependencies) = manifest.as_ref().and_then(|value| value.get("dependencies")).and_then(Value::as_object) {
            let root = join_path(target, "node_modules");
            if dependencies.keys().any(|name| self.managed_path(&root, name).is_ok_and(|path| !Path::new(&path).exists())) {
                self.git_dependencies(target).await?;
            }
        }
        Ok(())
    }

    async fn ensure_git_ref(&self, target: &str, fetch: Vec<String>, reference: &str) -> PackageResult<()> {
        self.run("git", fetch, Some(target), false).await?;
        let head = self.run("git", vec!["rev-parse".into(), "HEAD".into()], Some(target), true).await?;
        let commit = format!("{reference}^{{commit}}");
        let desired = self.run("git", vec!["rev-parse".into(), commit.clone()], Some(target), true).await?;
        let marker = git_marker(target);
        if head.trim() == desired.trim() && !Path::new(&marker).exists() {
            return self.repair_git_dependencies(target).await;
        }
        if head.trim() != desired.trim() {
            std::fs::write(&marker, "")?;
            // This command is a ported operation on manager-owned package clones only.
            self.run("git", vec!["reset".into(), "--hard".into(), commit], Some(target), false).await?;
        }
        if let Err(error) = self.run("git", vec!["clean".into(), "-fdx".into()], Some(target), false).await {
            if let Err(repair) = self.repair_git_dependencies(target).await { eprintln!("Failed to repair package dependencies: {repair}"); }
            return Err(error);
        }
        self.git_dependencies(target).await?;
        remove_file_if_exists(&marker)?;
        Ok(())
    }

    async fn git_update_target(&self, target: &str) -> PackageResult<(Vec<String>, String)> {
        let upstream = self.run("git", vec!["rev-parse".into(), "--abbrev-ref".into(), "@{upstream}".into()], Some(target), true).await;
        if let Ok(upstream) = upstream && let Some(branch) = upstream.trim().strip_prefix("origin/").filter(|branch| !branch.is_empty()) {
            if self.run("git", vec!["rev-parse".into(), "@{upstream}".into()], Some(target), true).await.is_ok() {
                return Ok((vec!["fetch".into(), "--prune".into(), "--no-tags".into(), "origin".into(), format!("+refs/heads/{branch}:refs/remotes/origin/{branch}")], "@{upstream}".into()));
            }
        }
        if let Err(error) = self.run("git", vec!["remote".into(), "set-head".into(), "origin".into(), "-a".into()], Some(target), false).await { eprintln!("Failed to refresh origin HEAD: {error}"); }
        self.run("git", vec!["rev-parse".into(), "origin/HEAD".into()], Some(target), true).await?;
        let symbolic = self.run("git", vec!["symbolic-ref".into(), "refs/remotes/origin/HEAD".into()], Some(target), true).await.unwrap_or_default();
        let branch = symbolic.trim().strip_prefix("refs/remotes/origin/").unwrap_or("");
        let refspec = if branch.is_empty() { "+HEAD:refs/remotes/origin/HEAD".into() } else { format!("+refs/heads/{branch}:refs/remotes/origin/{branch}") };
        Ok((vec!["fetch".into(), "--prune".into(), "--no-tags".into(), "origin".into(), refspec], "origin/HEAD".into()))
    }

    async fn install_parsed(&self, parsed: &ParsedSource, scope: SourceScope) -> PackageResult<()> {
        self.trusted(scope)?;
        match parsed {
            ParsedSource::Npm { spec, .. } => self.install_npm_specs(vec![spec.clone()], scope).await,
            ParsedSource::Git { repo, reference, .. } => {
                let target = self.managed_install_path(parsed, scope)?;
                if Path::new(&target).exists() {
                    let (fetch, reference) = match reference {
                        Some(reference) => (vec!["fetch".into(), "origin".into(), reference.clone()], "FETCH_HEAD".to_owned()),
                        None => self.git_update_target(&target).await?,
                    };
                    return self.ensure_git_ref(&target, fetch, &reference).await;
                }
                if scope != SourceScope::Temporary { ensure_ignore(&self.install_root("git", scope)?)?; }
                std::fs::create_dir_all(parent_path(&target))?;
                remove_file_if_exists(&git_marker(&target))?;
                let result = async {
                    self.run("git", vec!["clone".into(), repo.clone(), target.clone()], None, false).await?;
                    if let Some(reference) = reference { self.run("git", vec!["checkout".into(), reference.clone()], Some(&target), false).await?; }
                    self.git_dependencies(&target).await
                }.await;
                if result.is_err() { remove_dir_if_exists(&target)?; self.prune_git_parents(&target, scope)?; }
                result
            }
            ParsedSource::Local(path) => {
                let path = self.resolve_path(path, &self.cwd);
                if !Path::new(&path).exists() { return Err(PackageManagerError::Message(format!("Path does not exist: {path}"))); }
                Ok(())
            }
        }
    }

    pub async fn install(&self, source: &str, options: Option<PackageOptions>) -> PackageResult<()> {
        let scope = Self::scope(options); self.trusted(scope)?;
        self.emit(ProgressEventType::Start, ProgressAction::Install, source, Some(format!("Installing {source}...")));
        self.finish_progress(ProgressAction::Install, source, self.install_parsed(&parse_source(source), scope).await)
    }

    pub async fn install_and_persist(&mut self, source: &str, options: Option<PackageOptions>) -> PackageResult<()> {
        self.install(source, options).await?;
        self.add_source_to_settings(source, options)?;
        Ok(())
    }

    fn prune_git_parents(&self, target: &str, scope: SourceScope) -> PackageResult<()> {
        if scope == SourceScope::Temporary { return Ok(()); }
        let root = self.install_root("git", scope)?;
        let mut current = PathBuf::from(parent_path(target));
        while current != Path::new(&root) && current.starts_with(&root) {
            if current.exists() {
                if std::fs::read_dir(&current)?.next().is_some() { break; }
                std::fs::remove_dir(&current)?;
            }
            if !current.pop() { break; }
        }
        Ok(())
    }

    pub async fn remove(&self, source: &str, options: Option<PackageOptions>) -> PackageResult<()> {
        let scope = Self::scope(options); self.trusted(scope)?;
        self.emit(ProgressEventType::Start, ProgressAction::Remove, source, Some(format!("Removing {source}...")));
        let result = async {
            match parse_source(source) {
                ParsedSource::Npm { name, .. } => {
                    let root = self.install_root("npm", scope)?;
                    if !Path::new(&root).exists() { return Ok(()); }
                    let manager = Self::npm_name(&self.npm_command()?);
                    let mut args = vec!["uninstall".into(), name, if manager == "bun" { "--cwd".into() } else { "--prefix".into() }, root];
                    if manager != "bun" && manager != "pnpm" { args.push("--legacy-peer-deps".into()); }
                    self.npm(args, None, false).await?;
                }
                parsed @ ParsedSource::Git { .. } => {
                    let target = self.managed_install_path(&parsed, scope)?;
                    remove_dir_if_exists(&target)?; remove_file_if_exists(&git_marker(&target))?;
                    self.prune_git_parents(&target, scope)?;
                }
                ParsedSource::Local(_) => {}
            }
            Ok(())
        }.await;
        self.finish_progress(ProgressAction::Remove, source, result)
    }

    pub async fn remove_and_persist(&mut self, source: &str, options: Option<PackageOptions>) -> PackageResult<bool> {
        self.remove(source, options).await?;
        self.remove_source_from_settings(source, options)
    }

    fn offline(&self) -> bool {
        crate::brand::env_value("OFFLINE", &self.env).is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes"))
    }

    fn no_matching_package_message(&self, source: &str) -> String {
        let trimmed = source.trim();
        for scope in [SourceScope::User, SourceScope::Project] {
            for package in self.packages(scope) {
                let Some(configured) = source_string(&package) else { continue; };
                let suggested = match parse_source(configured) {
                    ParsedSource::Npm { name, spec, .. } => trimmed == name || trimmed == spec,
                    ParsedSource::Git { host, path, reference, .. } => {
                        let shorthand = format!("{host}/{path}");
                        trimmed == shorthand || reference.is_some_and(|reference| trimmed == format!("{shorthand}@{reference}"))
                    }
                    ParsedSource::Local(_) => false,
                };
                if suggested { return format!("No matching package found for {source}. Did you mean {configured}?"); }
            }
        }
        format!("No matching package found for {source}")
    }

    async fn should_update_npm(&self, parsed: &ParsedSource, scope: SourceScope) -> PackageResult<bool> {
        let ParsedSource::Npm { spec, name, version } = parsed else { return Ok(false); };
        let path = self.managed_install_path(parsed, scope)?;
        let Some(installed) = read_json(&join_path(&path, "package.json")).and_then(|value| value.get("version").and_then(Value::as_str).map(str::to_owned)) else { return Ok(true); };
        let output = self.npm(vec!["view".into(), if version.is_some() { spec.clone() } else { name.clone() }, "version".into(), "--json".into()], Some(&self.cwd), true).await;
        let Ok(output) = output else { return Ok(true); };
        let Ok(value) = serde_json::from_str::<Value>(&output) else { return Ok(true); };
        let versions = if let Some(version) = value.as_str() { vec![version.to_owned()] } else { strings(Some(&value)) };
        let latest = versions.iter().filter(|candidate| npm_matches(candidate, version.as_deref())).filter_map(|version| parse_version(version)).max();
        Ok(match (latest, parse_version(&installed)) { (Some(latest), Some(installed)) => latest > installed, _ => true })
    }

    pub async fn update(&self, source: Option<&str>) -> PackageResult<()> {
        use futures::StreamExt;
        let identity = source.map(|source| self.identity(source, None)).transpose()?;
        let mut matched = false;
        let mut user = Vec::new(); let mut project = Vec::new(); let mut git = Vec::new(); let mut npm = Vec::new();
        for scope in [SourceScope::User, SourceScope::Project] {
            for pkg in self.packages(scope) {
                let Some(configured) = source_string(&pkg) else { continue; };
                if let Some(identity) = &identity && self.identity(configured, Some(scope))? != *identity { continue; }
                matched = true;
                let parsed = parse_source(configured);
                match &parsed {
                    ParsedSource::Npm { spec, name, version } => {
                        if version.as_deref().is_some_and(|version| parse_version(version).is_some()) { continue; }
                        let spec = if version.is_some() { spec.clone() } else { format!("{name}@latest") };
                        npm.push((configured.to_owned(), spec, parsed, scope));
                    }
                    ParsedSource::Git { .. } => git.push((configured.to_owned(), parsed, scope)),
                    ParsedSource::Local(_) => {}
                }
            }
        }
        if source.is_some() && !matched {
            return Err(PackageManagerError::Message(self.no_matching_package_message(source.unwrap_or_default())));
        }
        if self.offline() { return Ok(()); }
        let checks: Vec<_> = futures::stream::iter(npm.into_iter().map(|(source, spec, parsed, scope)| async move {
            self.should_update_npm(&parsed, scope).await.map(|update| (source, spec, scope, update))
        })).buffered(4).collect().await;
        for check in checks {
            let (source, spec, scope, update) = check?;
            if update { if scope == SourceScope::User { user.push((source, spec)); } else { project.push((source, spec)); } }
        }
        let npm_batches = futures::future::try_join(self.update_npm_batch(user, SourceScope::User), self.update_npm_batch(project, SourceScope::Project));
        let git_updates = async {
            let results: Vec<_> = futures::stream::iter(git.into_iter().map(|(source, parsed, scope)| async move {
                self.emit(ProgressEventType::Start, ProgressAction::Update, &source, Some(format!("Updating {source}...")));
                self.finish_progress(ProgressAction::Update, &source, self.install_parsed(&parsed, scope).await)
            })).buffer_unordered(4).collect().await;
            for result in results { result?; }
            Ok::<(), PackageManagerError>(())
        };
        futures::future::try_join(npm_batches, git_updates).await?;
        Ok(())
    }

    async fn update_npm_batch(&self, sources: Vec<(String, String)>, scope: SourceScope) -> PackageResult<()> {
        if sources.is_empty() { return Ok(()); }
        let label = if sources.len() == 1 { sources[0].0.clone() } else { format!("{} npm packages", scope.as_str()) };
        self.emit(ProgressEventType::Start, ProgressAction::Update, &label, Some(format!("Updating {label}...")));
        let result = self.install_npm_specs(sources.into_iter().map(|(_, spec)| spec).collect(), scope).await;
        self.finish_progress(ProgressAction::Update, &label, result)
    }
}

fn parent_path(path: &str) -> String { Path::new(path).parent().map(|path| path.to_string_lossy().into_owned()).unwrap_or_default() }
fn git_marker(path: &str) -> String {
    let name = Path::new(path).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    join_path(&parent_path(path), &format!(".{name}.pi-update-incomplete"))
}
fn ensure_ignore(root: &str) -> PackageResult<()> {
    std::fs::create_dir_all(root)?;
    let path = join_path(root, ".gitignore");
    if !Path::new(&path).exists() { std::fs::write(path, "*\n!.gitignore\n")?; }
    Ok(())
}
fn remove_file_if_exists(path: &str) -> PackageResult<()> {
    match std::fs::remove_file(path) { Ok(()) => Ok(()), Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()), Err(error) => Err(error.into()) }
}
fn remove_dir_if_exists(path: &str) -> PackageResult<()> {
    match std::fs::remove_dir_all(path) { Ok(()) => Ok(()), Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()), Err(error) => Err(error.into()) }
}
fn read_json(path: &str) -> Option<Value> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(crate::text::strip_bom(&content)).ok()
}
fn parse_version(version: &str) -> Option<semver::Version> { semver::Version::parse(version.trim().trim_start_matches(['v', '='])).ok() }
fn npm_matches(version: &str, range: Option<&str>) -> bool {
    let Some(range) = range else { return true; };
    let Some(version) = parse_version(version) else { return false; };
    if let Some(exact) = parse_version(range) { return exact.cmp_precedence(&version).is_eq(); }
    let ranges: Vec<_> = range.split("||").collect();
    let mut recognized = false;
    for range in ranges {
        let range = range.trim();
        // npm joins comparators with whitespace; Rust semver uses commas.
        let normalized = if let Some((low, high)) = range.split_once(" - ") {
            let low = complete_version(low, false);
            let high = complete_version(high, true);
            format!(">={low}, <={high}")
        } else {
            let range = range.replace(">= ", ">=").replace("<= ", "<=").replace("> ", ">").replace("< ", "<").replace("~ ", "~").replace("^ ", "^");
            range.split_whitespace().map(|part| {
                let part = part.trim_start_matches('v');
                if part.chars().next().is_some_and(|c| c.is_ascii_digit()) && !part.contains(['-', '+']) {
                    if part.split('.').count() < 3 { format!("{part}.*") } else { format!("={part}") }
                } else {
                    let wildcard = part.replace(['x', 'X'], "*");
                    if wildcard.contains('*') { wildcard } else { part.to_owned() }
                }
            }).collect::<Vec<_>>().join(", ")
        };
        if let Ok(requirement) = semver::VersionReq::parse(&normalized) {
            recognized = true;
            if requirement.matches(&version) { return true; }
        }
    }
    // Dist-tags do not constrain the installed version.
    !recognized
}

fn complete_version(value: &str, upper: bool) -> String {
    let mut parts: Vec<_> = value.trim().split('.').map(str::to_owned).collect();
    while parts.len() < 3 { parts.push(if upper { u64::MAX.to_string() } else { "0".into() }); }
    parts.join(".")
}

const DATA_RESOURCE_TYPES: [ResourceType; 4] = [ResourceType::Skills, ResourceType::Prompts, ResourceType::Themes, ResourceType::Hooks];

#[cfg(test)]
#[path = "package_manager_tests.rs"]
mod manager_tests;

fn target_resources(paths: &mut ResolvedPaths, kind: ResourceType) -> &mut Vec<ResolvedResource> {
    match kind {
        ResourceType::Extensions => &mut paths.extensions,
        ResourceType::Skills => &mut paths.skills,
        ResourceType::Prompts => &mut paths.prompts,
        ResourceType::Themes => &mut paths.themes,
        ResourceType::Hooks => &mut paths.hooks,
    }
}

fn manifest_entries(manifest: &crate::pi_manifest::PiManifest, kind: ResourceType) -> Option<&[String]> {
    match kind {
        ResourceType::Extensions => manifest.extensions.as_deref(),
        ResourceType::Skills => manifest.skills.as_deref(),
        ResourceType::Prompts => manifest.prompts.as_deref(),
        ResourceType::Themes => manifest.themes.as_deref(),
        ResourceType::Hooks => manifest.hooks.as_deref(),
    }
}

fn resource_files(path: &str, kind: ResourceType) -> Vec<String> {
    if Path::new(path).is_file() { return vec![path.to_owned()]; }
    match kind {
        ResourceType::Skills => collect_skill_entries(path, SkillDiscoveryMode::Pi, None, None),
        ResourceType::Prompts | ResourceType::Themes | ResourceType::Hooks => collect_files(path, kind, true, None, None),
        ResourceType::Extensions => Vec::new(),
    }
}

fn auto_files(path: &str, kind: ResourceType, mode: SkillDiscoveryMode) -> Vec<String> {
    match kind {
        ResourceType::Skills => collect_auto_skill_entries(path, mode),
        ResourceType::Prompts | ResourceType::Themes => {
            let mut matcher = crate::skill_discovery::IgnoreMatcher::new();
            crate::skill_discovery::add_ignore_rules(&mut matcher, path, path);
            let Ok(entries) = std::fs::read_dir(path) else { return Vec::new(); };
            entries.flatten().filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') || matcher.ignores(&name) || !entry.path().is_file() || !file_pattern(kind).is_match(&name) { return None; }
                Some(entry.path().to_string_lossy().into_owned())
            }).collect()
        }
        ResourceType::Hooks => collect_files(path, kind, true, None, None),
        ResourceType::Extensions => Vec::new(),
    }
}

fn pattern_matches(path: &str, pattern: &str, base: &str, exact: bool) -> bool {
    let pattern = pattern.strip_prefix("./").or_else(|| pattern.strip_prefix(".\\")).unwrap_or(pattern).replace(std::path::MAIN_SEPARATOR, "/");
    let mut candidates = vec![relative_path(base, path), path.replace(std::path::MAIN_SEPARATOR, "/")];
    if !exact { candidates.push(Path::new(path).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()); }
    if Path::new(path).file_name().is_some_and(|name| name == "SKILL.md") {
        let parent = parent_path(path);
        candidates.extend([relative_path(base, &parent), parent.replace(std::path::MAIN_SEPARATOR, "/")]);
        if !exact { candidates.push(Path::new(&parent).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()); }
    }
    if exact { return candidates.iter().any(|candidate| candidate == &pattern); }
    let Ok(glob) = globset::GlobBuilder::new(&pattern).literal_separator(true).backslash_escape(true).build() else { return false; };
    let matcher = glob.compile_matcher();
    candidates.iter().any(|candidate| {
        // minimatch does not match a hidden segment unless the pattern names it.
        if candidate.split('/').any(|segment| segment.starts_with('.') && segment != "..") && !pattern.split('/').any(|segment| segment.starts_with('.')) { return false; }
        matcher.is_match(candidate)
    })
}

fn enabled_by_patterns(path: &str, patterns: &[String], base: &str) -> bool {
    let includes: Vec<_> = patterns.iter().filter(|pattern| !is_override_pattern(pattern)).collect();
    let mut enabled = includes.is_empty() || includes.iter().any(|pattern| pattern_matches(path, pattern, base, false));
    if patterns.iter().filter_map(|pattern| pattern.strip_prefix('!')).any(|pattern| pattern_matches(path, pattern, base, false)) { enabled = false; }
    if patterns.iter().filter_map(|pattern| pattern.strip_prefix('+')).any(|pattern| pattern_matches(path, pattern, base, true)) { enabled = true; }
    if patterns.iter().filter_map(|pattern| pattern.strip_prefix('-')).any(|pattern| pattern_matches(path, pattern, base, true)) { enabled = false; }
    enabled
}

fn delta_enabled(path: &str, patterns: &[String], base: &str) -> Option<bool> {
    let mut enabled = None;
    for pattern in patterns {
        let (target, exact, value) = match pattern.chars().next() {
            Some('+') => (&pattern[1..], true, true),
            Some('-') => (&pattern[1..], true, false),
            Some('!') => (&pattern[1..], false, false),
            Some(_) | None => (pattern.as_str(), false, true),
        };
        if pattern_matches(path, target, base, exact) { enabled = Some(value); }
    }
    enabled
}

fn expand_manifest_entries(entries: &[String], root: &str, kind: ResourceType) -> Vec<String> {
    let mut paths = Vec::new();
    for entry in entries.iter().filter(|entry| !is_override_pattern(entry)) {
        if has_glob_pattern(entry) {
            let mut matches = Vec::new();
            collect_glob_paths(root, root, entry, &mut matches);
            matches.sort();
            for path in matches { paths.extend(resource_files(&path, kind)); }
        } else { paths.extend(resource_files(&crate::paths::lexical_resolve(&join_path(root, entry)), kind)); }
    }
    paths
}

fn collect_glob_paths(dir: &str, root: &str, pattern: &str, paths: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return; };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with('.') { continue; }
        let path = entry.path().to_string_lossy().into_owned();
        let relative = relative_path(root, &path);
        if let Ok(glob) = globset::GlobBuilder::new(pattern).literal_separator(true).build() && glob.compile_matcher().is_match(&relative) { paths.push(path.clone()); }
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) { collect_glob_paths(&path, root, pattern, paths); }
    }
}

fn add_resource(target: &mut Vec<ResolvedResource>, path: String, enabled: bool, metadata: &PathMetadata) {
    if !path.is_empty() && !target.iter().any(|resource| resource.path == path) {
        target.push(ResolvedResource { path, enabled, metadata: metadata.clone() });
    }
}

fn finalize_paths(mut paths: ResolvedPaths) -> ResolvedPaths {
    for kind in DATA_RESOURCE_TYPES {
        let target = target_resources(&mut paths, kind);
        target.sort_by_key(|resource| resource_precedence_rank(&resource.metadata));
        let mut seen = std::collections::HashSet::new();
        target.retain(|resource| seen.insert(crate::paths::canonicalize_path(&resource.path)));
    }
    paths
}

impl DefaultPackageManager<'_> {
    fn collect_package_resources(&self, root: &str, filter: Option<&Value>, mut metadata: PathMetadata, paths: &mut ResolvedPaths) {
        let manifest = crate::pi_manifest::read_pi_manifest(&join_path(root, "package.json"));
        metadata.base_dir = Some(root.to_owned());
        if manifest.as_ref().is_some_and(|manifest| manifest.system == Some(true)) && metadata.scope == SourceScope::Temporary { metadata.scope = SourceScope::System; }
        for kind in DATA_RESOURCE_TYPES {
            let entries = manifest.as_ref().and_then(|manifest| manifest_entries(manifest, kind));
            let patterns = filter.and_then(|filter| filter.get(kind.as_str())).map(|value| strings(Some(value)));
            let delta = filter.is_some_and(|filter| filter.get("autoload").and_then(Value::as_bool) == Some(false));
            let filtered = delta || patterns.is_some();
            let all_files = if filtered {
                if let Some(entries) = entries.filter(|entries| !entries.is_empty()) {
                    expand_manifest_entries(entries, root, kind).into_iter().filter(|path| enabled_by_patterns(path, &entries.iter().filter(|entry| is_override_pattern(entry)).cloned().collect::<Vec<_>>(), root)).collect::<Vec<_>>()
                } else { resource_files(&join_path(root, kind.as_str()), kind) }
            } else if let Some(entries) = entries {
                expand_manifest_entries(entries, root, kind).into_iter().filter(|path| enabled_by_patterns(path, &entries.iter().filter(|entry| is_override_pattern(entry)).cloned().collect::<Vec<_>>(), root)).collect::<Vec<_>>()
            } else if manifest.is_none() || filter.is_some() {
                resource_files(&join_path(root, kind.as_str()), kind)
            } else { Vec::new() };
            for path in all_files {
                let enabled = if delta { delta_enabled(&path, patterns.as_deref().unwrap_or_default(), root) }
                else if let Some(patterns) = &patterns { Some(!patterns.is_empty() && enabled_by_patterns(&path, patterns, root)) }
                else { Some(true) };
                if let Some(enabled) = enabled { add_resource(target_resources(paths, kind), path, enabled, &metadata); }
            }
        }
    }

    fn dedupe_packages(&self, packages: Vec<(Value, SourceScope)>) -> PackageResult<Vec<(Value, SourceScope)>> {
        let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let mut result: Vec<(Value, SourceScope)> = Vec::new();
        for (pkg, scope) in packages {
            let Some(source) = source_string(&pkg) else { continue; };
            let identity = self.identity(source, Some(scope))?;
            if let Some(index) = seen.get(&identity).copied() {
                let (existing, existing_scope) = &result[index];
                if *existing_scope == SourceScope::Project && scope == SourceScope::User {
                    if existing.get("autoload").and_then(Value::as_bool) == Some(false) { result.push((pkg, scope)); }
                } else if scope == SourceScope::Project { result[index] = (pkg, scope); }
            } else { seen.insert(identity, result.len()); result.push((pkg, scope)); }
        }
        Ok(result)
    }

    async fn resolve_package_sources(&self, packages: &[(Value, SourceScope)], paths: &mut ResolvedPaths, on_missing: Option<&MissingSourceCallback>) -> PackageResult<()> {
        for (pkg, scope) in packages {
            let Some(source) = source_string(pkg) else { continue; };
            self.trusted(*scope)?;
            let mut resolved_source = source;
            let mut resolved_scope = *scope;
            if *scope == SourceScope::Project && pkg.get("autoload").and_then(Value::as_bool) == Some(false) {
                let identity = self.identity(source, Some(*scope))?;
                for (other, other_scope) in packages {
                    if *other_scope == SourceScope::User && let Some(other_source) = source_string(other) && self.identity(other_source, Some(*other_scope))? == identity {
                        resolved_source = other_source; resolved_scope = *other_scope; break;
                    }
                }
            }
            let parsed = parse_source(resolved_source);
            let mut installed = self.installed_path(&parsed, resolved_scope)?;
            match &parsed {
                ParsedSource::Local(_) => { if !Path::new(&installed).is_dir() { continue; } }
                ParsedSource::Npm { version, .. } | ParsedSource::Git { reference: version, .. } => {
                    let needs_install = !Path::new(&installed).exists() || match &parsed {
                        ParsedSource::Npm { .. } => read_json(&join_path(&installed, "package.json")).and_then(|value| value.get("version").and_then(Value::as_str).map(str::to_owned)).is_none_or(|installed| !npm_matches(&installed, version.as_deref())),
                        ParsedSource::Git { .. } | ParsedSource::Local(_) => false,
                    };
                    if needs_install {
                        if self.offline() { continue; }
                        let action = if let Some(callback) = on_missing { callback(resolved_source).await } else { MissingSourceAction::Install };
                        match action {
                            MissingSourceAction::Skip => continue,
                            MissingSourceAction::Error => return Err(PackageManagerError::Message(format!("Missing source: {resolved_source}"))),
                            MissingSourceAction::Install => self.install_parsed(&parsed, resolved_scope).await?,
                        }
                        installed = self.installed_path(&parsed, resolved_scope)?;
                    } else if matches!(&parsed, ParsedSource::Git { reference: None, .. }) && resolved_scope == SourceScope::Temporary && !self.offline() {
                        self.emit(ProgressEventType::Start, ProgressAction::Pull, resolved_source, Some(format!("Refreshing {resolved_source}...")));
                        let result = self.install_parsed(&parsed, resolved_scope).await;
                        if let Err(error) = self.finish_progress(ProgressAction::Pull, resolved_source, result) { eprintln!("Keeping cached temporary checkout: {error}"); }
                    }
                }
            }
            self.collect_package_resources(&installed, pkg.is_object().then_some(pkg), PathMetadata {
                source: source.to_owned(), scope: *scope, origin: SourceOrigin::Package, base_dir: Some(installed.clone()),
            }, paths);
        }
        Ok(())
    }

    pub async fn resolve_extension_sources(&self, sources: &[String], options: Option<ResolveSourcesOptions>) -> PackageResult<ResolvedPaths> {
        let options = options.unwrap_or_default();
        let scope = if options.temporary { SourceScope::Temporary } else if options.local { SourceScope::Project } else { SourceScope::User };
        let sources: Vec<_> = sources.iter().map(|source| (Value::String(source.clone()), scope)).collect();
        let mut paths = ResolvedPaths::default();
        self.resolve_package_sources(&sources, &mut paths, None).await?;
        Ok(finalize_paths(paths))
    }

    pub async fn resolve(&self, on_missing: Option<&MissingSourceCallback>) -> PackageResult<ResolvedPaths> {
        let mut packages = Vec::new();
        if self.settings_manager.is_project_trusted() { packages.extend(self.packages(SourceScope::Project).into_iter().map(|pkg| (pkg, SourceScope::Project))); }
        packages.extend(self.packages(SourceScope::User).into_iter().map(|pkg| (pkg, SourceScope::User)));
        let packages = self.dedupe_packages(packages)?;
        let mut paths = ResolvedPaths::default();
        self.resolve_package_sources(&packages, &mut paths, on_missing).await?;
        for scope in [SourceScope::Project, SourceScope::User] {
            if scope == SourceScope::Project && !self.settings_manager.is_project_trusted() { continue; }
            let settings = if scope == SourceScope::Project { self.settings_manager.get_project() } else { self.settings_manager.get_global() };
            let base = self.base_dir(scope)?;
            for kind in DATA_RESOURCE_TYPES {
                let entries = strings(settings.get(kind.as_str()));
                let split = split_patterns(&entries);
                let metadata = PathMetadata { source: "local".into(), scope, origin: SourceOrigin::TopLevel, base_dir: None };
                for entry in split.plain {
                    for path in resource_files(&self.resolve_path(&entry, &base), kind) {
                        let enabled = enabled_by_patterns(&path, &split.patterns, &base);
                        add_resource(target_resources(&mut paths, kind), path, enabled, &metadata);
                    }
                }
            }
        }
        self.add_auto_resources(&mut paths)?;
        Ok(finalize_paths(paths))
    }

    fn add_auto_resources(&self, paths: &mut ResolvedPaths) -> PackageResult<()> {
        let trusted = self.settings_manager.is_project_trusted();
        let project_base = join_path(&self.cwd, &crate::config::config_dir_name());
        let legacy = join_path(&self.cwd, ".pi");
        let user_agents = join_path(&self.home_dir, ".agents/skills");
        let mut locations = Vec::new();
        if trusted {
            locations.push((project_base.clone(), SourceScope::Project, None));
            for path in collect_ancestor_agents_skill_dirs(&self.cwd).into_iter().filter(|path| path != &user_agents) {
                locations.push((parent_path(&path), SourceScope::Project, Some(SkillDiscoveryMode::Agents)));
            }
        }
        // The pinned legacy .pi discovery is independent of the current config-dir trust gate.
        if legacy != project_base { locations.push((legacy, SourceScope::Project, None)); }
        locations.push((self.agent_dir.clone(), SourceScope::User, None));
        locations.push((parent_path(&user_agents), SourceScope::User, Some(SkillDiscoveryMode::Agents)));
        for (base, scope, agents) in locations {
            let settings = if scope == SourceScope::Project { self.settings_manager.get_project() } else { self.settings_manager.get_global() };
            let metadata = PathMetadata { source: "auto".into(), scope, origin: SourceOrigin::TopLevel, base_dir: Some(base.clone()) };
            for kind in DATA_RESOURCE_TYPES {
                if agents.is_some() && kind != ResourceType::Skills { continue; }
                let overrides: Vec<_> = strings(settings.get(kind.as_str())).into_iter().filter(|entry| is_override_pattern(entry)).collect();
                for path in auto_files(&join_path(&base, kind.as_str()), kind, agents.unwrap_or(SkillDiscoveryMode::Pi)) {
                    let enabled = enabled_by_patterns(&path, &overrides, &base);
                    add_resource(target_resources(paths, kind), path, enabled, &metadata);
                }
            }
        }
        Ok(())
    }
}

/// `PathMetadata`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PathMetadata {
    pub source: String,
    pub scope: SourceScope,
    pub origin: crate::source_info::SourceOrigin,
    pub base_dir: Option<String>,
}

/// `ResolvedResource`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedResource {
    pub path: String,
    pub enabled: bool,
    pub metadata: PathMetadata,
}

/// `ResolvedPaths`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResolvedPaths {
    pub extensions: Vec<ResolvedResource>,
    pub skills: Vec<ResolvedResource>,
    pub prompts: Vec<ResolvedResource>,
    pub themes: Vec<ResolvedResource>,
    pub hooks: Vec<ResolvedResource>,
}

/// `MissingSourceAction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingSourceAction {
    Install,
    Skip,
    Error,
}

/// `ProgressEvent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressEvent {
    pub event_type: ProgressEventType,
    pub action: ProgressAction,
    pub source: String,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressEventType {
    Start,
    Progress,
    Complete,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressAction {
    Install,
    Remove,
    Update,
    Clone,
    Pull,
}

/// `PackageUpdate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageUpdate {
    pub source: String,
    pub display_name: String,
    pub package_type: PackageUpdateType,
    pub scope: InstalledSourceScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageUpdateType {
    Npm,
    Git,
}

/// `InstalledSourceScope`: scopes whose packages the manager installs and updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstalledSourceScope {
    User,
    Project,
}

/// `ConfiguredPackage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfiguredPackage {
    pub source: String,
    pub scope: InstalledSourceScope,
    pub filtered: bool,
    pub installed_path: Option<String>,
}

/// `resourcePrecedenceRank`: lower rank wins a name collision.
pub fn resource_precedence_rank(metadata: &PathMetadata) -> u8 {
    if metadata.origin == crate::source_info::SourceOrigin::Package {
        return 4;
    }
    let scope_base = if metadata.scope == SourceScope::Project { 0 } else { 2 };
    scope_base + u8::from(metadata.source != "local")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_info::SourceOrigin;

    fn metadata(source: &str, scope: SourceScope, origin: SourceOrigin) -> PathMetadata {
        PathMetadata { source: source.to_string(), scope, origin, base_dir: None }
    }

    #[test]
    fn package_resources_rank_last() {
        assert_eq!(resource_precedence_rank(&metadata("local", SourceScope::Project, SourceOrigin::Package)), 4);
        assert_eq!(resource_precedence_rank(&metadata("auto", SourceScope::User, SourceOrigin::Package)), 4);
    }

    #[test]
    fn ranks_follow_project_then_user_then_discovered_order() {
        assert_eq!(resource_precedence_rank(&metadata("local", SourceScope::Project, SourceOrigin::TopLevel)), 0);
        assert_eq!(resource_precedence_rank(&metadata("auto", SourceScope::Project, SourceOrigin::TopLevel)), 1);
        assert_eq!(resource_precedence_rank(&metadata("local", SourceScope::User, SourceOrigin::TopLevel)), 2);
        assert_eq!(resource_precedence_rank(&metadata("auto", SourceScope::User, SourceOrigin::TopLevel)), 3);
        assert_eq!(resource_precedence_rank(&metadata("auto", SourceScope::Temporary, SourceOrigin::TopLevel)), 3);
    }
}

/// senpi RESOURCE_TYPES.
pub const RESOURCE_TYPES: [ResourceType; 5] = [
    ResourceType::Extensions,
    ResourceType::Skills,
    ResourceType::Prompts,
    ResourceType::Themes,
    ResourceType::Hooks,
];

/// senpi ResourceType.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceType {
    Extensions,
    Skills,
    Prompts,
    Themes,
    Hooks,
}

impl ResourceType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Extensions => "extensions",
            Self::Skills => "skills",
            Self::Prompts => "prompts",
            Self::Themes => "themes",
            Self::Hooks => "hooks",
        }
    }
}

/// senpi FILE_PATTERNS.
pub fn file_pattern(resource_type: ResourceType) -> &'static regex::Regex {
    use std::sync::LazyLock;
    static EXTENSIONS: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"\.(ts|js)$").expect("extensions pattern"));
    static MARKDOWN: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"\.md$").expect("markdown pattern"));
    static JSON: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"\.json$").expect("json pattern"));

    match resource_type {
        ResourceType::Extensions => &EXTENSIONS,
        ResourceType::Skills | ResourceType::Prompts => &MARKDOWN,
        ResourceType::Themes | ResourceType::Hooks => &JSON,
    }
}

/// senpi isPattern.
pub fn is_pattern(value: &str) -> bool {
    value.starts_with('!') || value.starts_with('+') || value.starts_with('-') || value.contains('*') || value.contains('?')
}

/// senpi isOverridePattern.
pub fn is_override_pattern(value: &str) -> bool {
    value.starts_with('!') || value.starts_with('+') || value.starts_with('-')
}

/// senpi hasGlobPattern.
pub fn has_glob_pattern(value: &str) -> bool {
    value.contains('*') || value.contains('?')
}

/// senpi splitPatterns.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SplitPatterns {
    pub plain: Vec<String>,
    pub patterns: Vec<String>,
}

pub fn split_patterns(entries: &[String]) -> SplitPatterns {
    let mut split = SplitPatterns::default();
    for entry in entries {
        if is_pattern(entry) {
            split.patterns.push(entry.clone());
        } else {
            split.plain.push(entry.clone());
        }
    }
    split
}

/// senpi getExtensionTempFolder: the 0700 temp folder extension packages install into.
pub fn get_extension_temp_folder(agent_dir: &str) -> String {
    let temp_folder = std::path::Path::new(agent_dir).join("tmp").join("extensions").to_string_lossy().into_owned();
    let _ = std::fs::create_dir_all(&temp_folder);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&temp_folder, std::fs::Permissions::from_mode(0o700));
    }
    temp_folder
}

/// senpi collectFiles: dot entries and node_modules are skipped, ignore files filter the walk.
pub fn collect_files(
    dir: &str,
    resource_type: ResourceType,
    skip_node_modules: bool,
    ignore_matcher: Option<&crate::skill_discovery::IgnoreMatcher>,
    root_dir: Option<&str>,
) -> Vec<String> {
    let mut files: Vec<String> = Vec::new();
    if !std::path::Path::new(dir).exists() {
        return files;
    }

    let root = root_dir.unwrap_or(dir).to_string();
    let mut matcher = ignore_matcher.cloned().unwrap_or_default();
    crate::skill_discovery::add_ignore_rules(&mut matcher, dir, &root);

    let Ok(entries) = std::fs::read_dir(dir) else {
        return files;
    };

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if skip_node_modules && name == "node_modules" {
            continue;
        }

        let full_path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let (is_dir, is_file) = if file_type.is_symlink() {
            match std::fs::metadata(&full_path) {
                Ok(metadata) => (metadata.is_dir(), metadata.is_file()),
                Err(_) => continue,
            }
        } else {
            (file_type.is_dir(), file_type.is_file())
        };

        let rel_path = full_path
            .strip_prefix(std::path::Path::new(&root))
            .map(|path| path.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/"))
            .unwrap_or_default();
        let ignore_path = if is_dir { format!("{rel_path}/") } else { rel_path };
        if matcher.ignores(&ignore_path) {
            continue;
        }

        if is_dir {
            files.extend(collect_files(&full_path.to_string_lossy(), resource_type, skip_node_modules, Some(&matcher), Some(&root)));
        } else if is_file && file_pattern(resource_type).is_match(&name) {
            files.push(full_path.to_string_lossy().into_owned());
        }
    }

    files
}

/// senpi findGitRepoRoot.
pub fn find_git_repo_root(start_dir: &str) -> Option<String> {
    let mut dir = crate::paths::lexical_resolve(start_dir);
    loop {
        if std::path::Path::new(&dir).join(".git").exists() {
            return Some(dir);
        }
        let parent = std::path::Path::new(&dir).parent().map(|parent| parent.to_string_lossy().into_owned());
        match parent {
            Some(parent) if parent != dir => dir = parent,
            _ => return None,
        }
    }
}

/// senpi collectAncestorAgentsSkillDirs.
pub fn collect_ancestor_agents_skill_dirs(start_dir: &str) -> Vec<String> {
    let mut skill_dirs: Vec<String> = Vec::new();
    let resolved_start_dir = crate::paths::lexical_resolve(start_dir);
    let git_repo_root = find_git_repo_root(&resolved_start_dir);

    let mut dir = resolved_start_dir;
    loop {
        skill_dirs.push(std::path::Path::new(&dir).join(".agents").join("skills").to_string_lossy().into_owned());
        if git_repo_root.as_deref() == Some(dir.as_str()) {
            break;
        }
        let parent = std::path::Path::new(&dir).parent().map(|parent| parent.to_string_lossy().into_owned());
        match parent {
            Some(parent) if parent != dir => dir = parent,
            _ => break,
        }
    }

    skill_dirs
}

#[cfg(test)]
mod discovery_tests {
    use super::*;
    use std::path::Path;

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(path, content).expect("write");
    }

    #[test]
    fn patterns_are_detected_by_their_markers() {
        assert!(is_pattern("!keep"));
        assert!(is_pattern("skills/*.md"));
        assert!(is_pattern("?x"));
        assert!(!is_pattern("plain/path.md"));
        assert!(is_override_pattern("-drop"));
        assert!(!is_override_pattern("skills/*.md"));
        assert!(has_glob_pattern("a*b"));
        assert!(!has_glob_pattern("a/b"));
    }

    #[test]
    fn split_patterns_keeps_order_within_each_bucket() {
        let entries = vec!["a".to_string(), "!b".to_string(), "c".to_string(), "d*".to_string()];
        let split = split_patterns(&entries);
        assert_eq!(split.plain, vec!["a".to_string(), "c".to_string()]);
        assert_eq!(split.patterns, vec!["!b".to_string(), "d*".to_string()]);
    }

    #[test]
    fn file_patterns_follow_the_resource_type() {
        assert!(file_pattern(ResourceType::Extensions).is_match("x.ts"));
        assert!(file_pattern(ResourceType::Extensions).is_match("x.js"));
        assert!(!file_pattern(ResourceType::Extensions).is_match("x.json"));
        assert!(file_pattern(ResourceType::Skills).is_match("x.md"));
        assert!(file_pattern(ResourceType::Prompts).is_match("x.md"));
        assert!(file_pattern(ResourceType::Themes).is_match("x.json"));
        assert!(file_pattern(ResourceType::Hooks).is_match("x.json"));
        assert_eq!(RESOURCE_TYPES.len(), 5);
        assert_eq!(ResourceType::Skills.as_str(), "skills");
    }

    #[test]
    fn collection_skips_dot_entries_and_node_modules() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write(&root.join("a.md"), "x");
        write(&root.join(".hidden.md"), "x");
        write(&root.join("node_modules").join("b.md"), "x");
        write(&root.join("nested").join("c.md"), "x");
        write(&root.join("nested").join("d.json"), "x");

        let files = collect_files(&root.to_string_lossy(), ResourceType::Skills, true, None, None);
        let names: Vec<String> = files.iter().map(|path| Path::new(path).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()).collect();
        assert!(names.contains(&"a.md".to_string()));
        assert!(names.contains(&"c.md".to_string()));
        assert!(!names.contains(&".hidden.md".to_string()));
        assert!(!names.contains(&"b.md".to_string()));
        assert!(!names.contains(&"d.json".to_string()));
    }

    #[test]
    fn an_ignore_file_filters_the_collection() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write(&root.join(".gitignore"), "skip.md\n");
        write(&root.join("skip.md"), "x");
        write(&root.join("keep.md"), "x");
        let files = collect_files(&root.to_string_lossy(), ResourceType::Skills, true, None, None);
        let names: Vec<String> = files.iter().map(|path| Path::new(path).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()).collect();
        assert_eq!(names, vec!["keep.md".to_string()]);
    }

    #[test]
    fn the_extension_temp_folder_is_created_with_owner_only_permissions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let folder = get_extension_temp_folder(&dir.path().to_string_lossy());
        assert!(folder.ends_with("tmp/extensions"));
        assert!(Path::new(&folder).is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&folder).expect("metadata").permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }
    }

    #[test]
    fn ancestor_agent_skill_dirs_stop_at_the_repository_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        let nested = repo.join("a").join("b");
        std::fs::create_dir_all(&nested).expect("mkdir");
        std::fs::create_dir_all(repo.join(".git")).expect("mkdir");
        let dirs = collect_ancestor_agents_skill_dirs(&nested.to_string_lossy());
        assert_eq!(dirs.len(), 3);
        assert!(dirs[0].ends_with("a/b/.agents/skills"));
        assert!(dirs[2].ends_with("repo/.agents/skills"));
    }
}
