//! Stable `mhc` launcher. It lives at the install root, reads `current.json` and hands over to the
//! payload at `versions/<current>/mhc`. Normal updates never replace it.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

const REINSTALL: &str = "reinstall mhc using the release installer";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pointer {
    schema_version: u32,
    current: String,
}

fn valid_version(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= 128
        && !version.starts_with('.')
        && version.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
}

fn install_root() -> Result<PathBuf, String> {
    let executable = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|error| format!("cannot locate launcher: {error}"))?;
    executable
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "launcher has no parent directory".to_owned())
}

fn payload(root: &Path) -> Result<PathBuf, String> {
    let pointer_path = root.join("current.json");
    let bytes = std::fs::read(&pointer_path)
        .map_err(|error| format!("cannot read {}: {error}; {REINSTALL}", pointer_path.display()))?;
    let pointer: Pointer = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid {}: {error}; {REINSTALL}", pointer_path.display()))?;
    if pointer.schema_version != 1 || !valid_version(&pointer.current) {
        return Err(format!("unsupported {}; {REINSTALL}", pointer_path.display()));
    }
    let target = root
        .join("versions")
        .join(&pointer.current)
        .join(format!("mhc{}", std::env::consts::EXE_SUFFIX));
    if !target.is_file() {
        return Err(format!("mhc {} is missing at {}; {REINSTALL}", pointer.current, target.display()));
    }
    Ok(target)
}

#[cfg(unix)]
fn launch(mut command: Command, target: &Path) -> Result<i32, String> {
    use std::os::unix::process::CommandExt;
    if let Some(argv0) = std::env::args_os().next() {
        command.arg0(argv0);
    }
    Err(format!("cannot execute {}: {}", target.display(), command.exec()))
}

#[cfg(windows)]
fn launch(mut command: Command, target: &Path) -> Result<i32, String> {
    // The child owns the console: keep Ctrl+C / Ctrl+Break from terminating the launcher first.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    let _runtime = runtime.enter();
    // without a console there is no Ctrl+C to forward.
    let _ctrl_c = tokio::signal::windows::ctrl_c().ok();
    let _ctrl_break = tokio::signal::windows::ctrl_break().ok();
    let status = command
        .status()
        .map_err(|error| format!("cannot run {}: {error}", target.display()))?;
    Ok(status.code().unwrap_or(1))
}

fn run() -> Result<i32, String> {
    let root = install_root()?;
    let target = payload(&root)?;
    let mut command = Command::new(&target);
    command.args(std::env::args_os().skip(1)).env("MHC_INSTALL_ROOT", &root);
    launch(command, &target)
}

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("mhc: {error}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_each_new_launch_from_current_pointer() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        for version in ["1.0.0", "1.1.0"] {
            let directory = root.path().join("versions").join(version);
            std::fs::create_dir_all(&directory)?;
            std::fs::write(directory.join(format!("mhc{}", std::env::consts::EXE_SUFFIX)), version)?;
        }
        let publish = |version: &str| -> Result<(), Box<dyn std::error::Error>> {
            std::fs::write(root.path().join("current.json"), serde_json::to_vec(
                &serde_json::json!({"schemaVersion": 1, "current": version, "previous": null}),
            )?)?;
            Ok(())
        };
        publish("1.0.0")?;
        let first = payload(root.path())?;
        let second = payload(root.path())?;
        publish("1.1.0")?;
        let third = payload(root.path())?;
        assert_eq!(first, second);
        assert_ne!(first, third);
        assert_eq!(std::fs::read(first)?, b"1.0.0");
        assert_eq!(std::fs::read(third)?, b"1.1.0");
        Ok(())
    }

    #[test]
    fn refuses_invalid_missing_or_unsupported_pointer() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        assert!(payload(root.path()).is_err());
        for body in [
            "not json",
            r#"{"schemaVersion":2,"current":"1.0.0"}"#,
            r#"{"schemaVersion":1,"current":"../outside"}"#,
            r#"{"schemaVersion":1,"current":"1.0.0"}"#,
        ] {
            std::fs::write(root.path().join("current.json"), body)?;
            assert!(payload(root.path()).is_err(), "{body}");
        }
        Ok(())
    }
}
