use crate::tool::types::EvalRuntimeInfo;
use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

/// Test-only host pin (`senpi#4660`): `SENPI_TEST_WORKSTATION={"cpuModel":string,"cores":int,"kernel":string}`
/// (with optional `"repoRoot":{"from":absolute path,"to":absolute path}` per `senpi#5067`).
pub const WORKSTATION_ENV: &str = "SENPI_TEST_WORKSTATION";

#[derive(Clone, Debug, PartialEq, Eq)]
struct PinnedRepoRoot {
    from: String,
    to: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PinnedWorkstation {
    cpu_model: String,
    cores: usize,
    repo_root: Option<PinnedRepoRoot>,
}

fn pinned_repo_root(value: &serde_json::Value) -> Option<PinnedRepoRoot> {
    let object = value.as_object()?;
    if object.len() != 2 {
        return None;
    }
    let from = object.get("from")?.as_str()?;
    let to = object.get("to")?.as_str()?;
    if !is_absolute_display_path(from) || !is_absolute_display_path(to) {
        return None;
    }
    Some(PinnedRepoRoot {
        from: from.to_owned(),
        to: to.to_owned(),
    })
}

fn is_absolute_display_path(raw: &str) -> bool {
    Path::new(raw).is_absolute() || raw.starts_with('/')
}

fn parse_pinned_workstation(raw: Option<&str>) -> Result<Option<PinnedWorkstation>, String> {
    let Some(value) = raw.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let parsed: Option<serde_json::Value> = serde_json::from_str(value).ok();
    if let Some(serde_json::Value::Object(map)) = &parsed {
        let valid_keys = match map.len() {
            3 => {
                map.contains_key("cores")
                    && map.contains_key("cpuModel")
                    && map.contains_key("kernel")
            }
            4 => {
                map.contains_key("cores")
                    && map.contains_key("cpuModel")
                    && map.contains_key("kernel")
                    && map.contains_key("repoRoot")
            }
            _ => false,
        };
        if valid_keys {
            let cpu_model = map.get("cpuModel").and_then(serde_json::Value::as_str);
            let kernel = map.get("kernel").and_then(serde_json::Value::as_str);
            let cores_num = map.get("cores").and_then(serde_json::Value::as_u64);
            let cores_is_int = map.get("cores").is_some_and(|v| v.is_u64() || v.is_i64());
            let repo_root = match map.get("repoRoot") {
                Some(raw_root) => pinned_repo_root(raw_root).map(Some),
                None => Some(None),
            };
            if let (Some(cpu_model), Some(kernel), Some(cores_u64), true, Some(repo_root)) =
                (cpu_model, kernel, cores_num, cores_is_int, repo_root)
                && !cpu_model.is_empty()
                && !kernel.is_empty()
                && let Ok(cores) = usize::try_from(cores_u64)
                && cores >= 1
            {
                return Ok(Some(PinnedWorkstation {
                    cpu_model: cpu_model.to_owned(),
                    cores,
                    repo_root,
                }));
            }
        }
    }
    Err(format!(
        "{WORKSTATION_ENV} must be a JSON object {{\"cpuModel\": non-empty string, \"cores\": positive integer, \"kernel\": non-empty string, optional \"repoRoot\": {{\"from\": absolute path, \"to\": absolute path}}}}, got {}",
        serde_json::to_string(value).unwrap_or_else(|_| format!("{value:?}"))
    ))
}

/// `file_path` as the eval prompt should show it: under the test-only `repoRoot` pin, a path inside
/// the pinned checkout (`from`) is shown under `to`. Only for rendering: reads keep using the real
/// path.
pub fn pinned_display_path(file_path: &str) -> Result<String, String> {
    let raw = maho_ext_api::process_env::var(WORKSTATION_ENV);
    pinned_display_path_in_raw(file_path, raw.as_deref())
}

pub fn pinned_display_path_in_env(
    file_path: &str,
    env: &HashMap<String, String>,
) -> Result<String, String> {
    pinned_display_path_in_raw(file_path, env.get(WORKSTATION_ENV).map(String::as_str))
}

pub fn pinned_display_path_in_raw(file_path: &str, raw: Option<&str>) -> Result<String, String> {
    let Some(root) = parse_pinned_workstation(raw)?.and_then(|pin| pin.repo_root) else {
        return Ok(file_path.to_owned());
    };
    if file_path == root.from {
        return Ok(root.to);
    }
    let sep = std::path::MAIN_SEPARATOR;
    let prefix = if root.from.ends_with('/') || root.from.ends_with(sep) {
        root.from.clone()
    } else {
        format!("{}/", root.from)
    };
    if let Some(rest) = file_path.strip_prefix(&prefix) {
        let base = root.to.trim_end_matches(['/', sep]);
        return Ok(if rest.is_empty() {
            base.to_owned()
        } else {
            format!("{base}/{rest}")
        });
    }
    Ok(file_path.to_owned())
}

/// The one-line host description the eval prompt shows (platform, CPU, cores, JS runtime).
pub fn host_line(js_runtime: &EvalRuntimeInfo) -> Result<String, String> {
    let raw = maho_ext_api::process_env::var(WORKSTATION_ENV);
    host_line_in_raw(Some(js_runtime), raw.as_deref())
}

pub fn host_line_in_env(
    js_runtime: Option<&EvalRuntimeInfo>,
    env: &HashMap<String, String>,
) -> Result<String, String> {
    host_line_in_raw(js_runtime, env.get(WORKSTATION_ENV).map(String::as_str))
}

pub fn host_line_in_raw(
    js_runtime: Option<&EvalRuntimeInfo>,
    raw: Option<&str>,
) -> Result<String, String> {
    let pinned = parse_pinned_workstation(raw)?;
    let platform = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        other => other,
    };
    let cpu = match &pinned {
        Some(pin) => Some(pin.cpu_model.clone()),
        None => CPU_MODEL.get_or_init(cpu_model).clone(),
    };
    let cores = match &pinned {
        Some(pin) => pin.cores,
        None => std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get),
    };
    let runtime = js_runtime
        .map(|runtime| format!("{} {}", runtime.name, runtime.version))
        .filter(|label| !label.trim().is_empty());
    Ok([
        Some(format!("{platform} {arch}")),
        cpu,
        Some(format!("{cores} cores")),
        runtime,
    ]
    .into_iter()
    .flatten()
    .filter(|part| !part.trim().is_empty())
    .collect::<Vec<_>>()
    .join(" \u{00b7} "))
}

/// The model id carried by a `model_select` event, when it has one.
pub fn model_id_from(event: &serde_json::Value) -> Option<String> {
    event.get("model")?.get("id")?.as_str().map(str::to_owned)
}

/// The CPU model never changes within a process; read it once instead of on every `TurnStart`.
static CPU_MODEL: OnceLock<Option<String>> = OnceLock::new();

fn cpu_model() -> Option<String> {
    if cfg!(target_os = "macos") {
        let output = std::process::Command::new("sysctl")
            .args(["-n", "machdep.cpu.brand_string"])
            .output()
            .ok()
            .filter(|output| output.status.success())?;
        return Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .filter(|model| !model.is_empty());
    }
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    cpuinfo
        .lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(key, _)| matches!(key.trim(), "model name" | "Hardware" | "Processor"))
                .map(|(_, value)| value.trim().to_owned())
        })
        .filter(|model| !model.is_empty())
}
