//! Test payload substituted for `mhc` in a packaged release. `probeVersion` is what `--version`
//! prints. `probeFailInstalled` exits 73 only after the file lives under `versions/<v>/`, so staging
//! can succeed and the updater's post-install check rolls back.

use std::path::Path;

fn main() {
    std::process::exit(match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("mhc-update-probe: {error}");
            1
        }
    });
}

fn run() -> Result<i32, String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let manifest_path = executable
        .parent()
        .ok_or("probe executable has no parent directory")?
        .join("manifest.json");
    let bytes = std::fs::read(&manifest_path)
        .map_err(|error| format!("read {}: {error}", manifest_path.display()))?;
    let manifest: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid {}: {error}", manifest_path.display()))?;
    let version = manifest
        .get("probeVersion")
        .and_then(serde_json::Value::as_str)
        .ok_or("manifest missing probeVersion")?;
    if let Some(marker) = std::env::var_os("MHC_PROBE_MARKER") {
        let path = executable.to_str().ok_or("probe path is not utf-8")?;
        std::fs::write(marker, path).map_err(|error| format!("write probe marker: {error}"))?;
    }
    let fail_installed =
        manifest.get("probeFailInstalled").and_then(serde_json::Value::as_bool) == Some(true);
    let in_versions = executable
        .parent()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .is_some_and(|name| name == "versions");
    if fail_installed && in_versions {
        return Ok(73);
    }
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--version") && args.next().is_none() {
        println!("{version}");
    }
    Ok(0)
}
