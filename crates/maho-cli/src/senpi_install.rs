//! Native counterpart of omo-senpi `src/install/`.
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct InstallOptions {
    pub home_dir: PathBuf,
    pub agent_dir: PathBuf,
    pub repo_root: PathBuf,
    pub plugin_path: PathBuf,
    pub cli_path: PathBuf,
    pub validate_artifacts: bool,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct InstallResult {
    pub ok: bool,
    pub action: &'static str,
    pub agent_dir: PathBuf,
    pub settings_path: PathBuf,
    pub plugin_path: PathBuf,
    pub launcher_path: Option<PathBuf>,
    pub changed: bool,
    pub backup_path: PathBuf,
    pub removed: Option<bool>,
}

const ARTIFACTS: &[&str] = &[
    "extensions/omo.js",
    "extensions/omo-task.js",
    "extensions/omo-computer-use.js",
    "extensions/omo-memory-doctor.js",
    "extensions/omo-memory-memfs.js",
    "extensions/omo-memory-palace.js",
    "extensions/omo-session-history.js",
    "extensions/omo-recall-store.js",
    "extensions/omo-telemetry-posthog.js",
    "extensions/omo-memory-sync.js",
    "extensions/loro_wasm_bg.wasm",
    "extensions/CaseFolding.txt",
    "extensions/omo-thread-client.js",
    "extensions/omo-feedback.js",
    "extensions/assets.generated.json",
    "extensions/omo-member.js",
    "extensions/memory-run-supervisor.mjs",
    "extensions/gateway-store-worker.mjs",
    "extensions/reflection-persona.md",
    "extensions/dream-persona.md",
    "extensions/facts-persona.md",
    "extensions/kibitzer-persona.md",
    "skills/ast-grep/SKILL.md",
    "skills/browser/SKILL.md",
    "skills/browser/runtime/omowright/index.js",
    "skills/browser/runtime/omowright/page-bundle.js",
    "skills/coding-agent-sessions/SKILL.md",
    "skills/debugging/SKILL.md",
    "skills/frontend/SKILL.md",
    "skills/git-master/SKILL.md",
    "skills/init-deep/SKILL.md",
    "skills/lsp-setup/SKILL.md",
    "skills/programming/SKILL.md",
    "skills/refactor/SKILL.md",
    "skills/remove-ai-slops/SKILL.md",
    "skills/review-work/SKILL.md",
    "skills/ultimate-browsing/SKILL.md",
    "skills/ultrawork/SKILL.md",
    "skills/ulw-execute/SKILL.md",
    "skills/ulw-loop/SKILL.md",
    "skills/ulw-plan/SKILL.md",
    "skills/ulw-research/SKILL.md",
    "skills/visual-qa/SKILL.md",
    "skills-conditional/x-search/SKILL.md",
    "runtime/agent-toolkit-sdk/sdk.js",
    "runtime/thread-sdk/sdk.js",
    "runtime/ast-grep-mcp/cli.js",
    "runtime/lsp-daemon/dist/cli.js",
    "runtime/lsp-daemon/dist/index.js",
    "runtime/lsp-daemon/dist/index.d.ts",
    "runtime/lsp-daemon/dist/daemon-client.js",
    "runtime/lsp-daemon/dist/daemon-client.d.ts",
    "runtime/lsp-daemon/dist/package.json",
    "runtime/lsp-daemon/dist/.omo-runtime-manifest.json",
    "scripts/install.mjs",
    "daemon-launch-spec.json",
];

pub fn install(options: &InstallOptions) -> Result<InstallResult, String> {
    if options.validate_artifacts {
        validate_plugin(&options.plugin_path)?;
    }
    let settings_path = options.agent_dir.join("settings.json");
    let mut settings = read_settings(&settings_path)?;
    let before = settings.clone();
    let mut packages = package_list(&settings)?;
    let mut seen = HashSet::new();
    packages.retain(|entry| {
        let full = resolve(&options.agent_dir, Path::new(entry));
        let shadows = ["packages/pi-goal", "packages/pi-webfetch"]
            .iter()
            .map(|p| resolve(&options.repo_root, Path::new(p)))
            .any(|p| p == full);
        let current = resolve(Path::new("."), &options.plugin_path);
        let package_name = fs::read_to_string(full.join("package.json"))
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .and_then(|v| v.get("name").and_then(Value::as_str).map(str::to_owned));
        !shadows
            && (full == current || package_name.as_deref() != Some("@code-yeongyu/omo-senpi"))
            && seen.insert(entry.clone())
    });
    let plugin = resolve(Path::new("."), &options.plugin_path)
        .to_string_lossy()
        .into_owned();
    if !packages.contains(&plugin) {
        packages.push(plugin.clone());
    }
    settings.insert(
        "packages".into(),
        Value::Array(packages.into_iter().map(Value::String).collect()),
    );
    let backup_path = write_settings(&settings_path, &settings)?;
    let launcher_path = install_launcher(options)?;
    Ok(InstallResult {
        ok: true,
        action: "install",
        agent_dir: options.agent_dir.clone(),
        settings_path,
        plugin_path: PathBuf::from(plugin),
        launcher_path,
        changed: settings != before,
        backup_path,
        removed: None,
    })
}

pub fn uninstall(options: &InstallOptions) -> Result<InstallResult, String> {
    let settings_path = options.agent_dir.join("settings.json");
    let mut settings = read_settings(&settings_path)?;
    let before = settings.clone();
    let packages = package_list(&settings)?;
    let plugin = resolve(Path::new("."), &options.plugin_path)
        .to_string_lossy()
        .into_owned();
    let next: Vec<_> = packages
        .into_iter()
        .filter(|entry| entry != &plugin)
        .collect();
    let removed = next.len() != package_list(&settings)?.len();
    settings.insert(
        "packages".into(),
        Value::Array(next.into_iter().map(Value::String).collect()),
    );
    let backup_path = write_settings(&settings_path, &settings)?;
    remove_launcher(&options.home_dir)?;
    Ok(InstallResult {
        ok: true,
        action: "uninstall",
        agent_dir: options.agent_dir.clone(),
        settings_path,
        plugin_path: PathBuf::from(plugin),
        launcher_path: None,
        changed: settings != before,
        backup_path,
        removed: Some(removed),
    })
}

fn read_settings(path: &Path) -> Result<Map<String, Value>, String> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .as_object()
            .cloned()
            .ok_or_else(|| format!("{} must contain a JSON object", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(e) => Err(e.to_string()),
    }
}

fn package_list(settings: &Map<String, Value>) -> Result<Vec<String>, String> {
    match settings.get("packages") {
        None => Ok(Vec::new()),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| "Senpi settings packages must be an array of strings".to_owned())
            })
            .collect(),
        _ => Err("Senpi settings packages must be an array of strings".into()),
    }
}

fn write_settings(path: &Path, settings: &Map<String, Value>) -> Result<PathBuf, String> {
    let parent = path.parent().ok_or("settings path has no parent")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    let base = format!("{}.{}.backup", path.display(), stamp);
    let mut backup = PathBuf::from(&base);
    for i in 1..1000 {
        if backup.exists() {
            backup = PathBuf::from(format!("{}-{i}.backup", base.trim_end_matches(".backup")));
        } else {
            break;
        }
    }
    if path.exists() {
        fs::copy(path, &backup).map_err(|e| e.to_string())?;
    } else {
        fs::write(&backup, "{}\n").map_err(|e| e.to_string())?;
    }
    let temp = path.with_extension(format!(
        "{}.{}.tmp",
        path.extension().and_then(|s| s.to_str()).unwrap_or("json"),
        std::process::id()
    ));
    fs::write(
        &temp,
        format!(
            "{}\n",
            serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?
        ),
    )
    .map_err(|e| e.to_string())?;
    fs::rename(temp, path).map_err(|e| e.to_string())?;
    Ok(backup)
}

fn validate_plugin(root: &Path) -> Result<(), String> {
    for artifact in ARTIFACTS {
        if !root.join(artifact).is_file() {
            return Err(format!(
                "Packed omo-senpi plugin is missing required runtime artifacts at {}",
                root.display()
            ));
        }
    }
    let runtime = root.join("runtime/ast-grep-mcp/cli.js");
    let manifest_path = root.join("runtime/ast-grep-mcp/manifest.json");
    let manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).map_err(|e| format!("Packed omo-senpi plugin ast-grep MCP runtime integrity error at {}: manifest is missing or unreadable: {e}", runtime.display()))?).map_err(|e| format!("Packed omo-senpi plugin ast-grep MCP runtime integrity error at {}: manifest is invalid: {e}", runtime.display()))?;
    let bytes = fs::read(&runtime).map_err(|e| format!("Packed omo-senpi plugin ast-grep MCP runtime integrity error at {}: runtime unreadable: {e}", runtime.display()))?;
    use sha2::Digest;
    let actual = format!("{:x}", sha2::Sha256::digest(bytes));
    if manifest.get("sha256").and_then(Value::as_str) != Some(&actual) {
        return Err(format!(
            "Packed omo-senpi plugin ast-grep MCP runtime integrity error at {}: sha256 mismatch",
            runtime.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Some(mode) = manifest.get("mode").and_then(Value::as_u64) {
            let actual_mode = fs::metadata(&runtime)
                .map_err(|e| e.to_string())?
                .permissions()
                .mode()
                & 0o777;
            if actual_mode != mode as u32 {
                return Err(format!(
                    "Packed omo-senpi plugin ast-grep MCP runtime integrity error at {}: mode mismatch",
                    runtime.display()
                ));
            }
        }
    }
    Ok(())
}

fn install_launcher(options: &InstallOptions) -> Result<Option<PathBuf>, String> {
    let path = options.home_dir.join(".local/bin/omo");
    if path.exists()
        && !fs::read_to_string(&path)
            .unwrap_or_default()
            .contains("omo-local-launcher")
    {
        return Ok(None);
    }
    let parent = path.parent().ok_or("launcher path has no parent")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let brand = serde_json::json!({"name":"OmO","command":"omo","displayVersion":"local","configDir":".omo","flatLayout":false,"envPrefix":"OMO","userAgent":"omo","originator":"omo","changelog":{"path":options.plugin_path.join("CHANGELOG.md")},"update":{"packageName":"omo-ai","distTag":"latest","command":"bun add -g omo-ai","changelogUrl":"https://github.com/code-yeongyu/oh-my-openagent/releases"}});
    let script = format!(
        "#!/usr/bin/env node\n// omo-local-launcher: generated by installer\nimport {{ spawn }} from 'node:child_process';\nimport {{ join }} from 'node:path';\nconst env={{...process.env,SENPI_BRAND:JSON.stringify({}),OMO_NATIVE:'1',OMO_CODING_AGENT_DIR:process.env.OMO_CODING_AGENT_DIR??process.env.SENPI_CODING_AGENT_DIR??process.env.PI_CODING_AGENT_DIR??join(process.env.HOME,'.omo','agent'),SENPI_CODING_AGENT_DIR:process.env.OMO_CODING_AGENT_DIR??process.env.SENPI_CODING_AGENT_DIR??process.env.PI_CODING_AGENT_DIR??join(process.env.HOME,'.omo','agent')}};\nenv.OMO_BIN=process.argv[1];\nconst child=spawn(process.execPath,[{},'--extension',{},...process.argv.slice(2)],{{env,stdio:'inherit'}});\nchild.on('close',(code,signal)=>{{if(signal)process.kill(process.pid,signal);else process.exitCode=code??1;}});\n",
        serde_json::to_string(&brand).map_err(|e| e.to_string())?,
        serde_json::to_string(&options.cli_path).map_err(|e| e.to_string())?,
        serde_json::to_string(&options.plugin_path).map_err(|e| e.to_string())?
    );
    fs::write(&path, script).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    Ok(Some(path))
}

fn remove_launcher(home: &Path) -> Result<(), String> {
    let path = home.join(".local/bin/omo");
    if path.exists()
        && fs::read_to_string(&path)
            .unwrap_or_default()
            .contains("omo-local-launcher")
    {
        fs::remove_file(path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn resolve(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn install_preserves_settings_and_deduplicates_plugin_path() {
        let root = tempfile::tempdir().expect("temp");
        let home = root.path().join("home");
        let agent = root.path().join("agent");
        let plugin = root.path().join("plugin");
        fs::create_dir_all(&agent).expect("agent");
        let initial_settings = serde_json::json!({
            "theme": "dark",
            "packages": ["keep", "keep", plugin.to_string_lossy()]
        });
        fs::write(
            agent.join("settings.json"),
            serde_json::to_string(&initial_settings).expect("settings json"),
        )
        .expect("settings");
        let options = InstallOptions {
            home_dir: home,
            agent_dir: agent.clone(),
            repo_root: root.path().into(),
            plugin_path: plugin.clone(),
            cli_path: root.path().join("cli.js"),
            validate_artifacts: false,
        };
        install(&options).expect("install");
        let value: Value =
            serde_json::from_slice(&fs::read(agent.join("settings.json")).expect("written"))
                .expect("json");
        assert_eq!(value["theme"], "dark");
        assert_eq!(
            value["packages"],
            serde_json::json!(["keep", plugin.to_string_lossy()])
        );
    }
}
