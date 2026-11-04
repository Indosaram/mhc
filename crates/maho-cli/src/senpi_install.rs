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
    "skills/ultrawork/references/commits.md",
    "skills/ultrawork/references/delegation-and-teams.md",
    "skills/ultrawork/references/qa-channels.md",
    "skills/ultrawork/references/review-gate.md",
    "skills/ultrawork/references/tui-visual-qa.md",
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

/// The OmO icon as a base64 SVG data URI: the `logo` the launcher puts in `SENPI_BRAND`
/// (upstream `omo-native/bin/lib/brand-logo.generated.js`, omo v5.1.52).
const OMO_BRAND_LOGO: &str = "data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCAxMDI0IDEwMjQiPgogIDxwYXRoIGZpbGw9IiNGNEY0RjQiIGQ9Ik01MTIuMDAgMTAwLjUwIEM1OTEuMTMgMTAwLjUwIDY5Ny42NiAxMDIuMzcgNzQ5LjM5IDEwNi4yNyBDODAxLjExIDExMC4xNyA4MDIuMzUgMTE1Ljg0IDgyMi4zNSAxMjMuOTAgQzg0Mi4zNCAxMzEuOTYgODU2LjQwIDE0MS42OCA4NjkuMzYgMTU0LjY0IEM4ODIuMzIgMTY3LjYwIDg5Mi4wNCAxODEuNjYgOTAwLjEwIDIwMS42NSBDOTA4LjE2IDIyMS42NSA5MTMuODMgMjIyLjg5IDkxNy43MyAyNzQuNjEgQzkyMS42MyAzMjYuMzQgOTIzLjUwIDQzMi44NyA5MjMuNTAgNTEyLjAwIEM5MjMuNTAgNTkxLjEzIDkyMS42MyA2OTcuNjYgOTE3LjczIDc0OS4zOSBDOTEzLjgzIDgwMS4xMSA5MDguMTYgODAyLjM1IDkwMC4xMCA4MjIuMzUgQzg5Mi4wNCA4NDIuMzQgODgyLjMyIDg1Ni40MCA4NjkuMzYgODY5LjM2IEM4NTYuNDAgODgyLjMyIDg0Mi4zNCA4OTIuMDQgODIyLjM1IDkwMC4xMCBDODAyLjM1IDkwOC4xNiA4MDEuMTEgOTEzLjgzIDc0OS4zOSA5MTcuNzMgQzY5Ny42NiA5MjEuNjMgNTkxLjEzIDkyMy41MCA1MTIuMDAgOTIzLjUwIEM0MzIuODcgOTIzLjUwIDMyNi4zNCA5MjEuNjMgMjc0LjYxIDkxNy43MyBDMjIyLjg5IDkxMy44MyAyMjEuNjUgOTA4LjE2IDIwMS42NSA5MDAuMTAgQzE4MS42NiA4OTIuMDQgMTY3LjYwIDg4Mi4zMiAxNTQuNjQgODY5LjM2IEMxNDEuNjggODU2LjQwIDEzMS45NiA4NDIuMzQgMTIzLjkwIDgyMi4zNSBDMTE1Ljg0IDgwMi4zNSAxMTAuMTcgODAxLjExIDEwNi4yNyA3NDkuMzkgQzEwMi4zNyA2OTcuNjYgMTAwLjUwIDU5MS4xMyAxMDAuNTAgNTEyLjAwIEMxMDAuNTAgNDMyLjg3IDEwMi4zNyAzMjYuMzQgMTA2LjI3IDI3NC42MSBDMTEwLjE3IDIyMi44OSAxMTUuODQgMjIxLjY1IDEyMy45MCAyMDEuNjUgQzEzMS45NiAxODEuNjYgMTQxLjY4IDE2Ny42MCAxNTQuNjQgMTU0LjY0IEMxNjcuNjAgMTQxLjY4IDE4MS42NiAxMzEuOTYgMjAxLjY1IDEyMy45MCBDMjIxLjY1IDExNS44NCAyMjIuODkgMTEwLjE3IDI3NC42MSAxMDYuMjcgQzMyNi4zNCAxMDIuMzcgNDMyLjg3IDEwMC41MCA1MTIuMDAgMTAwLjUwWiIvPgogIDxnIHRyYW5zZm9ybT0ic2NhbGUoMC41KSI+CiAgICA8ZyB0cmFuc2Zvcm09InRyYW5zbGF0ZSgwLjAwMDAwMCwyMDQ4LjAwMDAwMCkgc2NhbGUoMC4wNTAwMDAsLTAuMDUwMDAwKSI+CiAgICAgIDxwYXRoIGZpbGw9IiMwNDE2MTciIGZpbGwtcnVsZT0iZXZlbm9kZCIgZD0iTTkyNzAgMzIxNjcgYy0zMDkgLTY0IC00NTYgLTE1MiAtNjIxIC0zNzMgLTU2MiAtNzQ4IC03NzYgLTI2ODkKLTU0NyAtNDk4NCAxMTggLTExODcgMzI1IC0yNjAwIDU3MCAtMzg5MCAxNDMgLTc1MyAxNTIgLTY1MyAtMTI4IC0xMzgwIC00MDMKLTEwNDUgLTY0MyAtMTg3MyAtODEyIC0yODAwIC04NTMgLTQ2ODkgMTk3OSAtNzk4NiA3ODU4IC05MTQ4IDExNDAgLTIyNSAyNTE0Ci0zOTcgMzIwMiAtNDAwIGwzNjEgLTIgNzkgNTUgYzI2NCAxODIgMjEwIDY0MiAtMTExIDk0MiAtMzg3IDM2MiAtOTUwIDUzMgotMjgwMSA4NDUgLTMwMDQgNTA3IC00MjQ5IDk2NSAtNTUyMCAyMDMwIC0xMzU4IDExMzggLTIwMTIgMjg3OCAtMTgwOSA0ODEwCjU0NSA1MTk1IDY2MTcgODcyNiAxMzUyOSA3ODY2IDU1MDYgLTY4NCA5NDgwIC00MjU1IDk0ODAgLTg1MTggMCAtMjYwNyAtMTQ2MgotNDUyMiAtNDE1NyAtNTQ0OCAtOTI3IC0zMTggLTE1MzQgLTQ1OSAtMzIwMyAtNzQwIC0xODUxIC0zMTMgLTI0MTQgLTQ4MwotMjgwMSAtODQ1IC0zMjEgLTMwMCAtMzc1IC03NjAgLTExMSAtOTQyIGw3OSAtNTUgMzYxIDIgYzY4OCAzIDIwNjIgMTc1IDMyMDIKNDAwIDU4NzkgMTE2MiA4NzExIDQ0NTkgNzg1OCA5MTQ4IC0xNjkgOTI3IC00MDkgMTc1NSAtODEyIDI4MDAgLTI4MCA3MjcKLTI3MSA2MjcgLTEyOCAxMzgwIDI0NSAxMjkwIDQ1MiAyNzAzIDU3MCAzODkwIDI3MSAyNzIwIC03NyA0ODYxIC04NTUgNTI1OQotNzU4IDM4OCAtMjMxMCAtMjM4IC01MDIxIC0yMDI0IC04NjggLTU3MSAtNzY0IC01NTcgLTE0ODkgLTIxMSAtMzI3MSAxNTU4Ci02NzU1IDE1NTggLTEwMDI2IDAgLTcyNSAtMzQ2IC02MjEgLTM2MCAtMTQ4OSAyMTEgLTIzNzEgMTU2MiAtMzk1OSAyMjc4Ci00NzA4IDIxMjJ6IG00MTMyIC0zMzQ1IGMxNzUgLTE3NSAxMjQgLTI2OCAtNDA5IC03MzkgLTEwMjggLTkxMCAtMTk5NSAtMjA1MwotMjcxMyAtMzIwOSAtOTcgLTE1NiAtMjAwIC0zMTggLTIyOCAtMzU5IC0yMjEgLTMyMSAtNjQ2IC03NiAtNDY5IDI3MCAxNTMKMzAxIDg4MyAxMzM0IDEzODggMTk2NSA1NzggNzIzIDE0NjYgMTYxNCAyMDI5IDIwMzUgMTkxIDE0MiAyODggMTUxIDQwMiAzN3oKbTE0NTA3IC0xIGM0OTYgLTM0MSAxNDgyIC0xMzIzIDIwODAgLTIwNzEgNTA1IC02MzEgMTIzNSAtMTY2NCAxMzg4IC0xOTY1CjE3NyAtMzQ2IC0yNDggLTU5MSAtNDY5IC0yNzAgLTI4IDQxIC0xMzEgMjAzIC0yMjggMzU5IC03MTggMTE1NiAtMTY4NSAyMjk5Ci0yNzEzIDMyMDkgLTUzNSA0NzMgLTU4NSA1NjQgLTQwNyA3NDIgMTAwIDEwMCAxOTkgOTkgMzQ5IC00eiBNMTk5MTAgMjUzMDggYy00MDM5IC0yMDUgLTcyMTIgLTE2NjYgLTkwODYgLTQxODQgLTE4NDAgLTI0NzMgLTE3NTMKLTU3OTQgMTk2IC03NTQwIDExNjEgLTEwNDAgMjQ0NyAtMTUxOCA1NDIwIC0yMDE1IDEzNzQgLTIyOSAxOTk4IC0zODAgMjQ1NgotNTkxIDc1NCAtMzQ4IDExNjMgLTEwMDAgMTAzMyAtMTY1MCAtMTAgLTQ5IC0xIC03MiA0NyAtMTI0IGw2MCAtNjQgNDQ0IDAKNDQ0IDAgNjAgNjQgYzQ4IDUyIDU3IDc1IDQ3IDEyNCAtMTMwIDY1MCAyNzkgMTMwMiAxMDMzIDE2NTAgNDU4IDIxMSAxMDgyCjM2MiAyNDU2IDU5MSAyOTczIDQ5NyA0MjU5IDk3NSA1NDIwIDIwMTUgMjA4NCAxODY2IDIwMjYgNTM3NCAtMTMxIDc5NTYKLTIwNjMgMjQ2OSAtNTk5NSAzOTY2IC05ODk5IDM3Njh6IG0tNDY0NCAtNDM3NCBjMTM1NiAtMzYwIDIyNzIgLTE5MDkgMTk5OAotMzM3OCAtNDI2IC0yMjc4IC0yODU2IC0zMTQ0IC00NDI0IC0xNTc2IC0xNDk3IDE0OTYgLTkyNCA0MjE4IDEwMzEgNDg5OSAzOTMKMTM3IDk5NiAxNjEgMTM5NSA1NXogbTExNDgwIDM1IGMxNDkxIC0yNjEgMjUwNCAtMTkyMSAyMTU0IC0zNTMyIC00ODcgLTIyNDcKLTMxMDggLTMwMjQgLTQ1MjUgLTEzNDEgLTE3MTkgMjA0MiAtMTI5IDUzMTAgMjM3MSA0ODczeiBtLTY5NDggLTI5ODAgYzE3MgotNDAgMzQxIC0xMzMgNDk1IC0yNzMgOTUgLTg2IDE0MyAtMTE2IDE4NyAtMTE2IDQ0IDAgOTIgMzAgMTg3IDExNiA3MTggNjUwCjE3MTYgMTg4IDE4NTQgLTg2MCAzNSAtMjY3IDE4IC05NjggLTI1IC0xMDQ4IC0xMTkgLTIxOSAtMzUwIC0yNTEgLTQ5MSAtNjgKLTY4IDg5IC03MyAxMjQgLTg1IDY3MCAtMTMgNjIzIC0yNSA2NzUgLTIwMCA4MzggLTIxMiAxOTggLTUxNiAxOTkgLTczNSAyCi0xODAgLTE2MSAtMTkyIC0yMTIgLTIwNSAtODQwIC0xMiAtNTQ2IC0xNyAtNTgxIC04NSAtNjcwIC0xMjIgLTE1OSAtMzA4Ci0xNTkgLTQzMCAwIC02OCA4OSAtNzMgMTI0IC04NSA2NzAgLTEzIDYyOCAtMjUgNjc5IC0yMDUgODQwIC0yMTkgMTk3IC01MjMKMTk2IC03MzUgLTIgLTE3NSAtMTYzIC0xODcgLTIxNSAtMjAwIC04MzggLTEyIC01NDYgLTE3IC01ODEgLTg1IC02NzAgLTE0MQotMTgzIC0zNzIgLTE1MSAtNDkxIDY4IC03NCAxMzYgLTU0IDEwMzkgMzAgMTMzMiAxNzAgNTk0IDc1OCA5NzggMTMwNCA4NDl6IE0xNDM0MCAyMDI5MCBjLTE4NTggLTM3MiAtMjIzMCAtMzIyOCAtNTQ2IC00MTkxIDEyODAgLTczMiAyNzg2IDMzOAoyNzg2IDE5ODEgMCAxMzkxIC0xMDY4IDI0NDUgLTIyNDAgMjIxMHogTTI1OTkwIDIwMjkxIGMtMTczMyAtMzQzIC0yMjA4IC0yOTgxIC03MzIgLTQwNjkgMTI4NyAtOTQ5IDI5ODEgMTA4CjI5ODIgMTg2MCAwIDEzOTIgLTEwNzAgMjQ0MyAtMjI1MCAyMjA5eiIvPgogICAgPC9nPgogIDwvZz4KPC9zdmc+Cg==";

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
    let brand = serde_json::json!({"name":"OmO","logo":OMO_BRAND_LOGO,"command":"omo","displayVersion":"local","configDir":".omo","flatLayout":false,"envPrefix":"OMO","userAgent":"omo","originator":"omo","changelog":{"path":options.plugin_path.join("CHANGELOG.md")},"update":{"packageName":"omo-ai","distTag":"latest","command":"bun add -g omo-ai","changelogUrl":"https://github.com/code-yeongyu/oh-my-openagent/releases"}});
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

    #[test]
    fn launcher_brand_carries_the_omo_logo() {
        let root = tempfile::tempdir().expect("temp");
        let options = InstallOptions {
            home_dir: root.path().join("home"),
            agent_dir: root.path().join("agent"),
            repo_root: root.path().into(),
            plugin_path: root.path().join("plugin"),
            cli_path: root.path().join("cli.js"),
            validate_artifacts: false,
        };
        let path = install_launcher(&options).expect("launcher").expect("path");
        let script = fs::read_to_string(path).expect("script");
        assert!(script.contains("\"logo\":\"data:image/svg+xml;base64,"));
    }
}
