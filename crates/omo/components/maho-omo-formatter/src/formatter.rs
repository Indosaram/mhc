//! File-for-file native Rust port of upstream `formatter.ts`.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lsp_core::lsp::process::{SpawnPlatform, create_spawn_command};
use lsp_core::lsp::server_installation::{ResolveServerBinaryOptions, resolve_server_binary};
use maho_ext_api::{ToolContent, ToolResultEvent};
use maho_omo_post_mutation::{
    PostMutationEvent, extract_mutated_file_paths, is_mutation_tool_name, run_single_flight,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::fs;

// Mutation formatting is intentionally opt-in through project markers and fail-open by default.
pub const DEFAULT_FORMAT_MODE: FormatMode = FormatMode::BestEffort;
pub const DEFAULT_MAX_FILE_BYTES: u64 = 1_048_576; // 1 MB
pub const DEFAULT_TIMEOUT_MS: u64 = 3_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum FormatMode {
    Off,
    #[default]
    BestEffort,
    Required,
}

impl FormatMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::BestEffort => "best-effort",
            Self::Required => "required",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FormatterTool {
    Biome,
    Prettier,
    BunFormat,
    Rustfmt,
    Gofmt,
    Ruff,
}

impl FormatterTool {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Biome => "biome",
            Self::Prettier => "prettier",
            Self::BunFormat => "bun format",
            Self::Rustfmt => "rustfmt",
            Self::Gofmt => "gofmt",
            Self::Ruff => "ruff",
        }
    }
}

impl std::fmt::Display for FormatterTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Formatter {
    pub tool: FormatterTool,
    pub command: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatStatus {
    Formatted,
    Unchanged,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatResult {
    pub status: FormatStatus,
    pub added: usize,
    pub removed: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FormatOnMutationConfig {
    #[serde(default)]
    pub mode: Option<FormatMode>,
    #[serde(default)]
    pub languages: HashMap<String, bool>,
    #[serde(default, alias = "max_file_bytes")]
    pub max_file_bytes: Option<u64>,
    #[serde(default, alias = "timeout_ms")]
    pub timeout_ms: Option<u64>,
}

impl Default for FormatOnMutationConfig {
    fn default() -> Self {
        Self {
            mode: Some(DEFAULT_FORMAT_MODE),
            languages: HashMap::new(),
            max_file_bytes: Some(DEFAULT_MAX_FILE_BYTES),
            timeout_ms: Some(DEFAULT_TIMEOUT_MS),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct FormatMutationOutcome {
    pub content: Option<Vec<ToolContent>>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DaemonFormatResult {
    pub content: Option<Vec<ToolContent>>,
    pub details: Option<Value>,
    pub is_error: bool,
}

pub fn resolve_format_mode(config: &FormatOnMutationConfig, language: &str) -> FormatMode {
    if let Some(&false) = config.languages.get(language) {
        return FormatMode::Off;
    }
    config.mode.unwrap_or(DEFAULT_FORMAT_MODE)
}

pub fn detect_formatter(
    markers: &[String],
    file_path: &Path,
    config_text: &str,
) -> Option<Formatter> {
    let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");

    // Biome takes precedence over prettier
    if markers
        .iter()
        .any(|m| m == "biome.json" || m == "biome.jsonc")
    {
        return Some(Formatter {
            tool: FormatterTool::Biome,
            command: "biome".to_string(),
            args: vec!["format".to_string(), "--write".to_string()],
        });
    }

    // Prettier
    if markers.iter().any(|m| {
        m == ".prettierrc"
            || m.starts_with(".prettierrc")
            || m == "prettier.config.js"
            || m.starts_with("prettier.config.")
    }) {
        return Some(Formatter {
            tool: FormatterTool::Prettier,
            command: "prettier".to_string(),
            args: vec!["--write".to_string()],
        });
    }

    // Rustfmt
    if markers
        .iter()
        .any(|m| m == "rustfmt.toml" || m == "Cargo.toml")
        && ext == "rs"
    {
        return Some(Formatter {
            tool: FormatterTool::Rustfmt,
            command: "rustfmt".to_string(),
            args: vec![],
        });
    }

    // Gofmt
    if markers.iter().any(|m| m == "go.mod") && ext == "go" {
        return Some(Formatter {
            tool: FormatterTool::Gofmt,
            command: "gofmt".to_string(),
            args: vec!["-w".to_string()],
        });
    }

    // Ruff
    let has_ruff_marker = markers.iter().any(|m| m == "ruff.toml");
    let has_pyproject_ruff = markers.iter().any(|m| m == "pyproject.toml")
        && config_text
            .lines()
            .any(|l| l.trim_start().starts_with("[tool.ruff]"));

    if (has_ruff_marker || has_pyproject_ruff) && ext == "py" {
        return Some(Formatter {
            tool: FormatterTool::Ruff,
            command: "ruff".to_string(),
            args: vec!["format".to_string()],
        });
    }

    None
}

pub trait FormatterLogger: Send + Sync {
    fn warn(&self, message: &str);
}

impl<F: Fn(&str) + Send + Sync> FormatterLogger for F {
    fn warn(&self, message: &str) {
        self(message);
    }
}

pub type MarkersFn = Box<dyn Fn(&Path) -> Vec<String> + Send + Sync>;
pub type ReadMarkerFn = Box<dyn Fn(&Path, &str) -> String + Send + Sync>;
pub type DaemonFormatFn = Box<
    dyn Fn(&Path) -> Pin<Box<dyn Future<Output = Result<DaemonFormatResult, String>> + Send>>
        + Send
        + Sync,
>;
pub type ResolveBinaryFn = Box<dyn Fn(&str, &Path) -> Option<String> + Send + Sync>;

#[derive(Default)]
pub struct FormatterStepOptions {
    pub config: Option<FormatOnMutationConfig>,
    pub markers: Option<MarkersFn>,
    pub read_marker: Option<ReadMarkerFn>,
    pub daemon_format: Option<DaemonFormatFn>,
    pub resolve_binary: Option<ResolveBinaryFn>,
    pub logger: Option<Arc<dyn FormatterLogger>>,
}

/// Lexical path normalization matching Node `path.resolve(cwd, rawPath)`
pub fn normalize_resolved_path(cwd: &Path, raw_path: &str) -> PathBuf {
    let combined = if Path::new(raw_path).is_absolute() {
        PathBuf::from(raw_path)
    } else {
        cwd.join(raw_path)
    };

    let mut components = Vec::new();
    for c in combined.components() {
        match c {
            Component::Prefix(p) => components.push(Component::Prefix(p)),
            Component::RootDir => components.push(Component::RootDir),
            Component::CurDir => {}
            Component::ParentDir => {
                if let Some(last) = components.last() {
                    match last {
                        Component::Normal(_) => {
                            components.pop();
                        }
                        Component::RootDir => {}
                        _ => components.push(Component::ParentDir),
                    }
                } else {
                    components.push(Component::ParentDir);
                }
            }
            Component::Normal(n) => components.push(Component::Normal(n)),
        }
    }

    components.into_iter().collect()
}

pub fn language_for_path(path: &Path) -> String {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    match ext {
        "ts" | "tsx" => "typescript".to_string(),
        "py" => "python".to_string(),
        other => other.to_string(),
    }
}

pub async fn is_git_ignored(file_path: &Path, cwd: &Path) -> bool {
    let relative_path = match file_path.strip_prefix(cwd) {
        Ok(p) => p,
        Err(_) => file_path,
    };

    let mut cmd = tokio::process::Command::new("git");
    cmd.args(["check-ignore", "-q", "--"]);
    cmd.arg(relative_path);
    cmd.current_dir(cwd);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());

    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    match cmd.status().await {
        Ok(status) => status.success(),
        Err(_) => false,
    }
}

pub async fn default_packaged_daemon_format(
    file_path: &Path,
) -> Result<DaemonFormatResult, String> {
    let mut args = Map::new();
    args.insert("filePath".to_string(), json!(file_path.to_string_lossy()));

    let options = lsp_daemon::daemon_client::CallToolOptions::default();
    match lsp_daemon::daemon_client::call_tool_via_daemon("format", args, options).await {
        Ok(res) => {
            let content = res
                .content
                .into_iter()
                .filter_map(|v| serde_json::from_value::<ToolContent>(v).ok())
                .collect();
            Ok(DaemonFormatResult {
                content: Some(content),
                details: res.details,
                is_error: res.is_error,
            })
        }
        Err(err) => Err(err.to_string()),
    }
}

pub async fn format_one(
    file_path: &Path,
    formatter: &Formatter,
    cwd: &Path,
    timeout_ms: u64,
    daemon_format: Option<&DaemonFormatFn>,
    resolve_binary: Option<&ResolveBinaryFn>,
) -> FormatResult {
    // 1. Try daemon formatting if provided or default packaged daemon call
    let daemon_outcome = if let Some(df) = daemon_format {
        Some(df(file_path).await)
    } else {
        Some(default_packaged_daemon_format(file_path).await)
    };

    if let Some(Ok(daemon_res)) = daemon_outcome {
        if let Some(details) = daemon_res.details.as_ref().and_then(|d| d.as_object()) {
            if details.get("status").and_then(|s| s.as_str()) == Some("formatted") {
                let added = details
                    .get("linesAdded")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize;
                let removed = details
                    .get("linesRemoved")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize;
                return FormatResult {
                    status: FormatStatus::Formatted,
                    added,
                    removed,
                };
            }
            let reason = details.get("reason").and_then(|r| r.as_str());
            let status = details.get("status").and_then(|s| s.as_str());
            if reason != Some("capability_not_advertised") && status != Some("unavailable") {
                return FormatResult {
                    status: FormatStatus::Unchanged,
                    added: 0,
                    removed: 0,
                };
            }
        } else {
            // Daemon succeeded with missing/non-object details: upstream treats details as undefined
            // and returns unchanged (reason/status are both undefined, neither is capability_not_advertised or unavailable)
            return FormatResult {
                status: FormatStatus::Unchanged,
                added: 0,
                removed: 0,
            };
        }
    }

    // 2. CLI fallback: resolve binary with fallback to resolveServerBinary
    let binary = if let Some(rb) = resolve_binary {
        rb(&formatter.command, cwd).or_else(|| {
            let opts = ResolveServerBinaryOptions::default();
            resolve_server_binary(
                std::slice::from_ref(&formatter.command),
                Some(&cwd.to_string_lossy()),
                &opts,
            )
        })
    } else {
        let opts = ResolveServerBinaryOptions::default();
        resolve_server_binary(
            std::slice::from_ref(&formatter.command),
            Some(&cwd.to_string_lossy()),
            &opts,
        )
    };

    let Some(binary_path) = binary else {
        return FormatResult {
            status: FormatStatus::Missing,
            added: 0,
            removed: 0,
        };
    };

    let before = match fs::read_to_string(file_path).await {
        Ok(text) => text,
        Err(_) => {
            return FormatResult {
                status: FormatStatus::Unchanged,
                added: 0,
                removed: 0,
            };
        }
    };

    let mut cmd_parts = vec![binary_path];
    cmd_parts.extend(formatter.args.clone());
    cmd_parts.push(file_path.to_string_lossy().into_owned());

    #[cfg(windows)]
    let platform = SpawnPlatform::Windows;
    #[cfg(not(windows))]
    let platform = SpawnPlatform::Unix;

    let env_map: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    let prepared = match create_spawn_command(&cmd_parts, platform, "cmd.exe", &env_map) {
        Ok(p) => p,
        Err(_) => {
            return FormatResult {
                status: FormatStatus::Missing,
                added: 0,
                removed: 0,
            };
        }
    };

    let mut tokio_cmd = tokio::process::Command::new(&prepared.command);
    tokio_cmd.args(&prepared.args);
    tokio_cmd.current_dir(cwd);
    tokio_cmd.stdin(std::process::Stdio::null());
    tokio_cmd.stdout(std::process::Stdio::null());
    tokio_cmd.stderr(std::process::Stdio::null());

    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        tokio_cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = match tokio_cmd.spawn() {
        Ok(c) => c,
        Err(_) => {
            return FormatResult {
                status: FormatStatus::Missing,
                added: 0,
                removed: 0,
            };
        }
    };

    let exit_res = tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait()).await;
    match exit_res {
        Ok(Ok(status)) => {
            if !status.success() {
                let _ = fs::write(file_path, &before).await;
                return FormatResult {
                    status: FormatStatus::Unchanged,
                    added: 0,
                    removed: 0,
                };
            }
        }
        Ok(Err(_)) => {
            return FormatResult {
                status: FormatStatus::Missing,
                added: 0,
                removed: 0,
            };
        }
        Err(_) => {
            let _ = child.kill().await;
            let _ = fs::write(file_path, &before).await;
            return FormatResult {
                status: FormatStatus::Unchanged,
                added: 0,
                removed: 0,
            };
        }
    }

    let after = match fs::read_to_string(file_path).await {
        Ok(text) => text,
        Err(_) => {
            return FormatResult {
                status: FormatStatus::Unchanged,
                added: 0,
                removed: 0,
            };
        }
    };

    if after == before {
        return FormatResult {
            status: FormatStatus::Unchanged,
            added: 0,
            removed: 0,
        };
    }

    // Exact split('\n').count math matching upstream split('\n').length
    let before_lines = before.split('\n').count();
    let after_lines = after.split('\n').count();
    let added = after_lines.saturating_sub(before_lines);
    let removed = before_lines.saturating_sub(after_lines);

    FormatResult {
        status: FormatStatus::Formatted,
        added,
        removed,
    }
}

#[derive(Clone)]
pub struct FormatterStep {
    config: FormatOnMutationConfig,
    markers: Option<Arc<MarkersFn>>,
    read_marker: Option<Arc<ReadMarkerFn>>,
    daemon_format: Option<Arc<DaemonFormatFn>>,
    resolve_binary: Option<Arc<ResolveBinaryFn>>,
    logger: Option<Arc<dyn FormatterLogger>>,
    missing_notices: Arc<Mutex<HashSet<String>>>,
}

impl FormatterStep {
    pub fn new(options: FormatterStepOptions) -> Self {
        Self {
            config: options.config.unwrap_or_default(),
            markers: options.markers.map(Arc::new),
            read_marker: options.read_marker.map(Arc::new),
            daemon_format: options.daemon_format.map(Arc::new),
            resolve_binary: options.resolve_binary.map(Arc::new),
            logger: options.logger,
            missing_notices: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub async fn format_tool_result(
        &self,
        event: &ToolResultEvent,
        cwd: &Path,
        session_id: Option<&str>,
    ) -> FormatMutationOutcome {
        let post_event = PostMutationEvent::from(event);
        self.format_mutation(&post_event, cwd, session_id).await
    }

    pub async fn format_mutation(
        &self,
        event: &PostMutationEvent,
        cwd: &Path,
        session_id: Option<&str>,
    ) -> FormatMutationOutcome {
        if !is_mutation_tool_name(&event.tool_name) {
            return FormatMutationOutcome::default();
        }

        let mode = self.config.mode.unwrap_or(FormatMode::BestEffort);
        if mode == FormatMode::Off {
            return FormatMutationOutcome::default();
        }

        let max_bytes = self.config.max_file_bytes.unwrap_or(DEFAULT_MAX_FILE_BYTES);
        let timeout_ms = self.config.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
        let sid = session_id.unwrap_or("anonymous");

        let mut additions: Vec<ToolContent> = Vec::new();
        let mut required_error: Option<String> = None;

        let raw_paths = extract_mutated_file_paths(event);
        let mut marker_ctx: Option<(Vec<String>, String)> = None;
        for raw_path in raw_paths {
            // Lexical normalization matching upstream resolve(cwd, rawPath)
            let file_path = normalize_resolved_path(cwd, &raw_path);

            if !file_path.exists() {
                continue;
            }

            let meta = match std::fs::metadata(&file_path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.len() > max_bytes {
                continue;
            }

            // Upstream: options.markers?.(cwd) ?? []
            // If options.markers is None, default to EMPTY vec (no invented list_dir_markers)
            // PERF-DIVERGENCE(omo formatter): upstream lists the cwd and reads pyproject.toml for every mutated path; we compute them lazily once per call, after the first path passes the exists/size gates. The formatter writes only its target file, so the results are identical.
            let (marker_names, pyproject_text) = marker_ctx.get_or_insert_with(|| {
                let marker_names = if let Some(m_fn) = &self.markers {
                    m_fn(cwd)
                } else {
                    Vec::new()
                };
                let pyproject_text = if let Some(rm_fn) = &self.read_marker {
                    rm_fn(cwd, "pyproject.toml")
                } else {
                    String::new()
                };
                (marker_names, pyproject_text)
            });

            let Some(mut marker) = detect_formatter(marker_names, &file_path, pyproject_text)
            else {
                continue;
            };

            let language = language_for_path(&file_path);
            if resolve_format_mode(&self.config, &language) == FormatMode::Off {
                continue;
            }

            // Checked after the side-effect-free formatter/mode gates so no `git` process is
            // spawned for files that would never be formatted.
            if is_git_ignored(&file_path, cwd).await {
                continue;
            }

            if marker.tool == FormatterTool::Prettier
                && let Some(command) = resolve_bun_format(cwd).await {
                    marker = Formatter {
                        tool: FormatterTool::BunFormat,
                        command,
                        args: vec!["format".into(), "--write".into()],
                    };
                }
            let fp_str = file_path.to_string_lossy().into_owned();
            let fp_clone = file_path.clone();
            let marker_clone = marker.clone();
            let cwd_clone = cwd.to_path_buf();
            let daemon_format_clone = self.daemon_format.clone();
            let resolve_binary_clone = self.resolve_binary.clone();

            let result = run_single_flight(&fp_str, || async move {
                format_one(
                    &fp_clone,
                    &marker_clone,
                    &cwd_clone,
                    timeout_ms,
                    daemon_format_clone.as_deref(),
                    resolve_binary_clone.as_deref(),
                )
                .await
            })
            .await;

            if result.status == FormatStatus::Formatted {
                let rel_path = file_path.strip_prefix(cwd).unwrap_or(&file_path);
                let rel_display = rel_path.to_string_lossy().replace('\\', "/");
                let text = format!(
                    "\n\n(OmO) auto-formatted {} with {} (+{}/-{} lines). File content changed; re-read before exact-text edits.",
                    rel_display, marker.tool, result.added, result.removed
                );
                additions.push(ToolContent::Text {
                    text,
                    audience: None,
                });
            }

            if result.status == FormatStatus::Missing && mode == FormatMode::Required {
                required_error = Some(format!(
                    "Formatter {} is unavailable for {}",
                    marker.tool, raw_path
                ));
            }

            if result.status == FormatStatus::Missing && mode == FormatMode::BestEffort {
                let notice_key = format!("{}:{}", sid, marker.tool);
                let mut notices = self
                    .missing_notices
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let (true, Some(logger)) = (notices.insert(notice_key), &self.logger) {
                    logger.warn(&format!(
                        "Formatter {} unavailable; install it with bun add -d",
                        marker.tool
                    ));
                }
            }
        }

        FormatMutationOutcome {
            content: if additions.is_empty() {
                None
            } else {
                Some(additions)
            },
            error: required_error,
        }
    }
}

pub fn create_formatter_step(options: FormatterStepOptions) -> FormatterStep {
    FormatterStep::new(options)
}

/// Resolve only Bun's native formatter, never a same-named project script or executable.
pub async fn resolve_bun_format(cwd: &Path) -> Option<String> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    let mut root = cwd;
    loop {
        if home.as_deref() == Some(root) {
            return None;
        }
        let manifest = read_package_json(root);
        if ["bun.lock", "bun.lockb", "bunfig.toml"]
            .iter()
            .any(|name| root.join(name).exists())
            || manifest
                .get("packageManager")
                .and_then(Value::as_str)
                .is_some_and(|s| s.starts_with("bun@"))
        {
            break;
        }
        if root.join(".git").exists() {
            return None;
        }
        root = root.parent()?;
    }
    let mut dir = cwd;
    let mut found_manifest = false;
    loop {
        if !found_manifest && dir.join("package.json").exists() {
            found_manifest = true;
            if read_package_json(dir)
                .pointer("/scripts/format")
                .and_then(Value::as_str)
                .is_some()
            {
                return None;
            }
        }
        let suffixes: &[&str] = if cfg!(windows) {
            &[".exe", ".bunx", ".cmd", ""]
        } else {
            &[""]
        };
        if suffixes.iter().any(|suffix| {
            dir.join("node_modules/.bin")
                .join(format!("format{suffix}"))
                .exists()
        }) {
            return None;
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => break,
        }
    }
    let extensions = [".tsx", ".ts", ".jsx", ".cts", ".cjs", ".js", ".mjs", ".mts"];
    if cwd.join("format").is_file()
        || extensions
            .iter()
            .any(|ext| cwd.join(format!("format{ext}")).is_file())
        || (cwd.join("format").is_dir()
            && (cwd.join("format/package.json").is_file()
                || extensions
                    .iter()
                    .any(|ext| cwd.join("format").join(format!("index{ext}")).is_file())))
    {
        return None;
    }
    let binary = resolve_server_binary(
        &["bun".into()],
        Some(&cwd.to_string_lossy()),
        &ResolveServerBinaryOptions::default(),
    )?;
    let probe = tempfile::tempdir().ok()?;
    let mut help = tokio::process::Command::new(&binary);
    help.args(["format", "--help"])
        .current_dir(probe.path())
        .kill_on_drop(true);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    if !tokio::time::timeout_at(deadline, help.output())
        .await
        .ok()?
        .ok()?
        .status
        .success()
    {
        return None;
    }
    let file = probe.path().join("probe.ts");
    fs::write(&file, "const a=1\n").await.ok()?;
    let mut write = tokio::process::Command::new(&binary);
    write
        .args(["format", "--write", "probe.ts"])
        .current_dir(probe.path())
        .kill_on_drop(true);
    if !tokio::time::timeout_at(deadline, write.output())
        .await
        .ok()?
        .ok()?
        .status
        .success()
    {
        return None;
    }
    if fs::read_to_string(file).await.ok()? == "const a=1\n" {
        return None;
    }
    Some(binary)
}
fn read_package_json(dir: &Path) -> Value {
    std::fs::read(dir.join("package.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null)
}

#[cfg(all(test, unix))]
mod bun_format_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[tokio::test]
    async fn resolves_only_native_bun_and_proves_in_place_rewrite() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("bun.lock"), "").unwrap();
        // The resolver's binary search accepts a project-local executable.
        let bin = root.path().join("node_modules/.bin");
        std::fs::create_dir_all(&bin).unwrap();
        let bun = bin.join("bun");
        std::fs::write(&bun, "#!/bin/sh\nif [ \"$2\" = --help ]; then exit 0; fi\nprintf 'const a = 1;\\n' > probe.ts\n").unwrap();
        std::fs::set_permissions(&bun, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(resolve_bun_format(root.path()).await.is_some());
        std::fs::write(
            root.path().join("package.json"),
            r#"{"scripts":{"format":"prettier"}}"#,
        )
        .unwrap();
        assert!(resolve_bun_format(root.path()).await.is_none());
        std::fs::remove_file(root.path().join("package.json")).unwrap();
        std::fs::write(root.path().join("format.ts"), "").unwrap();
        assert!(resolve_bun_format(root.path()).await.is_none());
        std::fs::remove_file(root.path().join("format.ts")).unwrap();
        std::fs::write(&bun, "#!/bin/sh\nexit 0\n").unwrap();
        assert!(resolve_bun_format(root.path()).await.is_none());
    }
}
