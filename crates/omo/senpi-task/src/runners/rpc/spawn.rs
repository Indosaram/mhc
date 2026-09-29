//! `runners/rpc/spawn.ts`: the argv/env for one senpi RPC child (and its model-catalog probe).

use std::collections::BTreeMap;
use std::path::{MAIN_SEPARATOR, Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use crate::runners::rpc::process::RpcSpawnDescriptor;
use crate::runners::types::RpcRunnerSpec;
use crate::senpi::as_senpi_thinking_level;

const SESSION_DIR_ENV: &str = "SENPI_CODING_AGENT_SESSION_DIR";
const SENPI_BIN_ENV: &str = "SENPI_BIN";
const RPC_ENTRY_RELATIVE: &str = "node_modules/@code-yeongyu/senpi/dist/rpc-entry.js";
/// `team/member-extension/identity.ts` (owned by the team slice).
const MEMBER_PROCESS_ENV_NAMES: [&str; 3] = [
    "SENPI_TASK_MEMBER",
    "SENPI_TASK_MEMBER_TASK_ID",
    "SENPI_TASK_TEAM_CONFIG",
];
const MEMBER_EXTENSION_BUNDLE_NAME: &str = "omo-member.js";

pub type ExecutableResolver = Arc<dyn Fn(&RpcSpawnRuntime) -> Option<String> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SenpiLauncher {
    pub command: String,
    pub prefix_args: Vec<String>,
}

/// The process facts spawn resolution depends on; tests inject every field.
#[derive(Clone)]
pub struct RpcSpawnRuntime {
    pub is_bun_binary: bool,
    pub exec_path: String,
    /// Node `process.platform` spelling (`linux`, `darwin`, `win32`).
    pub platform: String,
    pub parent_env: BTreeMap<String, String>,
    pub resolve_rpc_entry: Arc<dyn Fn() -> String + Send + Sync>,
    pub resolve_senpi_executable: Option<ExecutableResolver>,
}

impl Default for RpcSpawnRuntime {
    /// The Rust host is not a Node process, so the `execPath + rpc-entry` fallback launches `node`
    /// against the senpi package found by walking up from the working directory.
    fn default() -> Self {
        Self {
            is_bun_binary: false,
            exec_path: "node".to_string(),
            platform: node_platform().to_string(),
            parent_env: std::env::vars().collect(),
            resolve_rpc_entry: Arc::new(default_rpc_entry),
            resolve_senpi_executable: None,
        }
    }
}

fn node_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

fn default_rpc_entry() -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    cwd.ancestors()
        .map(|dir| dir.join(RPC_ENTRY_RELATIVE))
        .find(|candidate| candidate.exists())
        .unwrap_or_else(|| PathBuf::from(RPC_ENTRY_RELATIVE))
        .to_string_lossy()
        .into_owned()
}

/// A Bun compiled binary's module URL carries a `$bunfs` / `~BUN` marker.
pub fn detect_bun_binary(meta_url: &str) -> bool {
    meta_url.contains("$bunfs") || meta_url.contains("~BUN") || meta_url.contains("%7EBUN")
}

/// The isolated session dir for a child, nested under the senpi-task state dir (trailing separator).
pub fn resolve_child_session_dir(state_dir: &str, task_id: &str) -> String {
    let joined = absolute(&Path::new(state_dir).join("sessions").join(task_id));
    format!("{}{MAIN_SEPARATOR}", joined.to_string_lossy())
}

fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

fn senpi_binary_name(platform: &str) -> &'static str {
    if platform == "win32" {
        "senpi.exe"
    } else {
        "senpi"
    }
}

fn scan_path_for_executable(name: &str, path_value: Option<&str>) -> Option<String> {
    std::env::split_paths(path_value.unwrap_or(""))
        .filter(|dir| !dir.as_os_str().is_empty())
        .find_map(|dir| canonical_executable(&dir.join(name)))
}

fn canonical_executable(candidate: &Path) -> Option<String> {
    let canonical = std::fs::canonicalize(absolute(candidate)).ok()?;
    canonical
        .is_file()
        .then(|| canonical.to_string_lossy().into_owned())
}

/// Resolve the senpi executable: an explicit `SENPI_BIN`, the sibling of a Bun-compiled senpi, then
/// a PATH scan. `None` lets [`build_rpc_spawn`] fall back to `execPath + rpc-entry`.
pub fn resolve_senpi_executable(runtime: &RpcSpawnRuntime) -> Option<String> {
    let binary_name = senpi_binary_name(&runtime.platform);
    let override_value = runtime
        .parent_env
        .get(SENPI_BIN_ENV)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty());
    if let Some(value) = override_value {
        if value.contains('/') || value.contains(MAIN_SEPARATOR) || Path::new(value).is_absolute() {
            return canonical_executable(Path::new(value));
        }
        return scan_path_for_executable(value, runtime.parent_env.get("PATH").map(String::as_str));
    }
    if runtime.is_bun_binary {
        let dir = Path::new(&runtime.exec_path)
            .parent()
            .unwrap_or_else(|| Path::new(""));
        return canonical_executable(&dir.join(binary_name));
    }
    scan_path_for_executable(
        binary_name,
        runtime.parent_env.get("PATH").map(String::as_str),
    )
}

fn normalize_senpi_launcher(executable: &str, runtime: &RpcSpawnRuntime) -> Option<SenpiLauncher> {
    if runtime.platform != "win32" || executable.to_lowercase().ends_with(".exe") {
        return Some(SenpiLauncher {
            command: executable.to_string(),
            prefix_args: Vec::new(),
        });
    }
    let shim_dir = Path::new(executable)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let package_cli = ["@code-yeongyu", "senpi", "dist", "cli.js"];
    let candidates = [
        package_cli
            .iter()
            .fold(shim_dir.join("node_modules"), |path, part| path.join(part)),
        package_cli.iter().fold(
            shim_dir.parent().unwrap_or(shim_dir).to_path_buf(),
            |path, part| path.join(part),
        ),
    ];
    candidates
        .iter()
        .find(|candidate| candidate.exists())
        .map(|cli| SenpiLauncher {
            command: runtime.exec_path.clone(),
            prefix_args: vec![cli.to_string_lossy().into_owned()],
        })
}

pub fn resolve_senpi_launcher(runtime: &RpcSpawnRuntime) -> Option<SenpiLauncher> {
    let executable = match &runtime.resolve_senpi_executable {
        Some(resolver) => resolver(runtime),
        None => resolve_senpi_executable(runtime),
    };
    if let Some(normalized) = executable
        .as_deref()
        .and_then(|executable| normalize_senpi_launcher(executable, runtime))
    {
        return Some(normalized);
    }
    if runtime.platform != "win32" {
        return None;
    }
    let path = runtime.parent_env.get("PATH").map(String::as_str);
    ["senpi.cmd", "senpi"].iter().find_map(|name| {
        scan_path_for_executable(name, path)
            .and_then(|shim| normalize_senpi_launcher(&shim, runtime))
    })
}

/// A dag child (state dir `<state>/children/<task_id>` with a `dag` owner record) drops the first
/// inherited extension. An unreadable record counts as not dag-owned.
fn is_dag_owned_child(spec: &RpcRunnerSpec) -> bool {
    let state_dir = Path::new(&spec.state_dir);
    let parent = state_dir.parent();
    let is_child_dir = parent
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        == Some("children")
        && state_dir.file_name().and_then(|name| name.to_str()) == Some(spec.task_id.as_str());
    let Some(root) = parent.and_then(Path::parent).filter(|_| is_child_dir) else {
        return false;
    };
    let record_path = root.join("tasks").join(format!("{}.json", spec.task_id));
    std::fs::read_to_string(record_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|record| {
            record
                .pointer("/owner/kind")
                .and_then(Value::as_str)
                .map(|kind| kind == "dag")
        })
        .unwrap_or(false)
}

/// `--no-extensions`, then each threaded `--extension`, then `--model`, then `--thinking`.
pub fn build_child_args(spec: &RpcRunnerSpec) -> Vec<String> {
    let mut args = vec!["--no-extensions".to_string()];
    let extensions = spec.extensions.as_deref().unwrap_or_default();
    let extensions = if is_dag_owned_child(spec) {
        extensions.get(1..).unwrap_or_default()
    } else {
        extensions
    };
    push_extensions(&mut args, extensions);
    if let Some(model) = spec.model.as_deref().filter(|model| !model.is_empty()) {
        args.extend(["--model".to_string(), model.to_string()]);
    }
    let reasoning = spec.reasoning.as_deref().or(spec.variant.as_deref());
    if let Some(level) = as_senpi_thinking_level(reasoning) {
        args.extend(["--thinking".to_string(), level.as_str().to_string()]);
    }
    args
}

pub fn build_model_catalog_args(spec: &RpcRunnerSpec) -> Vec<String> {
    let mut args = vec!["--no-extensions".to_string()];
    push_extensions(&mut args, spec.extensions.as_deref().unwrap_or_default());
    args.extend(
        [
            "--no-skills",
            "--no-prompt-templates",
            "--no-context-files",
            "--list-models",
        ]
        .map(str::to_string),
    );
    args
}

fn push_extensions(args: &mut Vec<String>, extensions: &[String]) {
    for entry in extensions.iter().filter(|entry| !entry.is_empty()) {
        args.extend(["--extension".to_string(), entry.clone()]);
    }
}

/// The env/extension preamble shared by the real child and the catalog probe: member identity is
/// stripped before explicit `member_env` applies, and the session dir is always ours.
fn build_child_profile(
    spec: &RpcRunnerSpec,
    runtime: &RpcSpawnRuntime,
) -> (BTreeMap<String, String>, RpcRunnerSpec) {
    let mut env = runtime.parent_env.clone();
    for name in MEMBER_PROCESS_ENV_NAMES {
        env.remove(name);
    }
    if let Some(member_env) = &spec.member_env {
        env.extend(member_env.clone());
    }
    env.insert(
        SESSION_DIR_ENV.to_string(),
        resolve_child_session_dir(&spec.state_dir, &spec.task_id),
    );
    let mut profiled = spec.clone();
    if spec.member_env.is_none()
        && let Some(extensions) = &mut profiled.extensions
    {
        extensions.retain(|entry| {
            Path::new(entry).file_name().and_then(|name| name.to_str())
                != Some(MEMBER_EXTENSION_BUNDLE_NAME)
        });
    }
    (env, profiled)
}

pub fn build_rpc_spawn(spec: &RpcRunnerSpec, runtime: &RpcSpawnRuntime) -> RpcSpawnDescriptor {
    let (env, profiled) = build_child_profile(spec, runtime);
    let child_args = build_child_args(&profiled);
    if let Some(launcher) = resolve_senpi_launcher(runtime) {
        let mut args = launcher.prefix_args;
        args.extend(["--mode".to_string(), "rpc".to_string()]);
        args.extend(child_args);
        return RpcSpawnDescriptor {
            command: launcher.command,
            args,
            cwd: spec.cwd.clone(),
            env,
        };
    }
    let mut args = vec![(runtime.resolve_rpc_entry)()];
    args.extend(child_args);
    RpcSpawnDescriptor {
        command: runtime.exec_path.clone(),
        args,
        cwd: spec.cwd.clone(),
        env,
    }
}

pub fn build_rpc_model_catalog_spawn(
    spec: &RpcRunnerSpec,
    runtime: &RpcSpawnRuntime,
) -> RpcSpawnDescriptor {
    let (env, profiled) = build_child_profile(spec, runtime);
    let child_args = build_model_catalog_args(&profiled);
    if let Some(launcher) = resolve_senpi_launcher(runtime) {
        let mut args = launcher.prefix_args;
        args.extend(child_args);
        return RpcSpawnDescriptor {
            command: launcher.command,
            args,
            cwd: spec.cwd.clone(),
            env,
        };
    }
    let entry = (runtime.resolve_rpc_entry)();
    let cli_entry = Path::new(&entry)
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .join("cli.js");
    let mut args = vec![cli_entry.to_string_lossy().into_owned()];
    args.extend(child_args);
    RpcSpawnDescriptor {
        command: runtime.exec_path.clone(),
        args,
        cwd: spec.cwd.clone(),
        env,
    }
}
