use std::path::Path;

#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn matches_installation(executable: &Path, installation: &Path) -> bool {
    executable.starts_with(installation)
}

#[cfg(target_os = "linux")]
pub async fn ensure_unused(installation: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir("/proc").map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == std::process::id() {
            continue;
        }
        match std::fs::read_link(entry.path().join("exe")) {
            Ok(executable) if matches_installation(&executable, installation) => {
                return Err(format!(
                    "installation is in use by process {pid}; close it before updating"
                ));
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) => {}
            Err(error) => return Err(format!("cannot inspect process {pid}: {error}")),
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub async fn ensure_unused(installation: &Path) -> Result<(), String> {
    let mut command = tokio::process::Command::new("/usr/sbin/lsof");
    command
        .args(["-n", "-P", "-d", "txt", "-Fpn"])
        .kill_on_drop(true);
    let output = inventory_output(command).await?;
    if !output.status.success() {
        return Err("process inventory failed; refusing update".into());
    }
    let listing = String::from_utf8(output.stdout).map_err(|error| error.to_string())?;
    let mut pid = None;
    for line in listing.lines() {
        if let Some(value) = line.strip_prefix('p') {
            pid = Some(value.parse::<u32>().map_err(|error| error.to_string())?);
        } else if let Some(name) = line.strip_prefix('n')
            && pid.is_some_and(|pid| pid != std::process::id())
            && matches_installation(Path::new(name), installation)
        {
            return Err(format!(
                "installation is in use by process {}; close it before updating",
                pid.expect("checked process id")
            ));
        }
    }
    Ok(())
}

#[cfg(windows)]
pub async fn ensure_unused(installation: &Path) -> Result<(), String> {
    ensure_unused_except(installation, std::process::id()).await
}

#[cfg(windows)]
pub async fn ensure_unused_except(installation: &Path, updater: u32) -> Result<(), String> {
    let mut command = tokio::process::Command::new("powershell.exe");
    command.args(["-NoProfile", "-NonInteractive", "-Command", "$ErrorActionPreference='Stop'; [Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false); @(Get-CimInstance Win32_Process | Select-Object ProcessId,ExecutablePath) | ConvertTo-Json -Compress"])
        .kill_on_drop(true);
    let output = inventory_output(command).await?;
    if !output.status.success() {
        return Err("process inventory failed; refusing update".into());
    }
    let processes = parse_windows_inventory(&output.stdout)?;
    for process in processes {
        if process.process_id == std::process::id() || process.process_id == updater {
            continue;
        }
        if let Some(executable) = process.executable_path
            && windows_installation_match(&executable, installation)
        {
            return Err(format!(
                "installation is in use by process {}; close it before updating",
                process.process_id
            ));
        }
    }
    Ok(())
}

#[cfg(any(windows, test))]
#[derive(serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WindowsProcess {
    process_id: u32,
    executable_path: Option<String>,
}

#[cfg(any(windows, test))]
fn parse_windows_inventory(bytes: &[u8]) -> Result<Vec<WindowsProcess>, String> {
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(Vec::new());
    }
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum ProcessList {
        Multiple(Vec<WindowsProcess>),
        Single(WindowsProcess),
    }
    let list: Option<ProcessList> =
        serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    Ok(match list {
        Some(ProcessList::Multiple(v)) => v,
        Some(ProcessList::Single(p)) => vec![p],
        None => Vec::new(),
    })
}

#[cfg(windows)]
fn windows_installation_match(executable: &str, installation: &Path) -> bool {
    fn normalize(path: &str) -> String {
        let path = path.replace('/', "\\").to_lowercase();
        if let Some(unc) = path.strip_prefix(r"\\?\unc\") {
            return format!(r"\\{}", unc.trim_end_matches('\\'));
        }
        path.strip_prefix(r"\\?\")
            .unwrap_or(&path)
            .trim_end_matches('\\')
            .to_owned()
    }
    let executable = normalize(executable);
    let installation = normalize(&installation.to_string_lossy());
    executable
        .strip_prefix(&installation)
        .is_some_and(|rest| rest.starts_with('\\'))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub async fn ensure_unused(_installation: &Path) -> Result<(), String> {
    Err("native updates are unsupported on this platform".into())
}

#[cfg(any(target_os = "macos", windows))]
async fn inventory_output(
    mut command: tokio::process::Command,
) -> Result<std::process::Output, String> {
    tokio::time::timeout(std::time::Duration::from_secs(30), command.output())
        .await
        .map_err(|_| "process inventory timed out; refusing update".to_owned())?
        .map_err(|error| format!("cannot inspect running executables: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_inventory_accepts_powershell_json_shapes() -> Result<(), String> {
        for bytes in [b"".as_slice(), b" \r\n", b"null", b"[]"] {
            assert!(parse_windows_inventory(bytes)?.is_empty());
        }
        for bytes in [
            br#"{"ProcessId":42,"ExecutablePath":"C:\\mhc\\mhc.exe"}"#.as_slice(),
            br#"[{"ProcessId":42,"ExecutablePath":"C:\\mhc\\mhc.exe"}]"#,
            b"\xef\xbb\xbf{\"ProcessId\":42,\"ExecutablePath\":\"C:\\\\mhc\\\\mhc.exe\"}",
        ] {
            let processes = parse_windows_inventory(bytes)?;
            assert_eq!(processes.len(), 1);
            assert_eq!(processes[0].process_id, 42);
            assert_eq!(
                processes[0].executable_path.as_deref(),
                Some(r"C:\mhc\mhc.exe")
            );
        }
        assert!(
            parse_windows_inventory(br#"[{"ProcessId":43,"ExecutablePath":null}]"#)?[0]
                .executable_path
                .is_none()
        );
        for bytes in [
            b"garbage".as_slice(),
            b"{}",
            b"[{}]",
            b"{\"ProcessId\":\"42\"}",
        ] {
            assert!(parse_windows_inventory(bytes).is_err());
        }
        Ok(())
    }

    #[test]
    fn installation_match_uses_path_components_not_string_prefixes() {
        let root = Path::new("packages").join("mhc");
        assert!(matches_installation(&root.join("mhc"), &root));
        assert!(matches_installation(&root.join("lsp-daemon"), &root));
        assert!(!matches_installation(
            &Path::new("packages").join("mhc-other/mhc"),
            &root
        ));
    }

    #[cfg(windows)]
    #[test]
    fn process_paths_match_verbatim_prefix_and_case_without_reopening_process_files() {
        let installation = Path::new(r"\\?\C:\Users\Example\mhc");
        assert!(windows_installation_match(
            r"c:\users\example\MHC\mhc.exe",
            installation
        ));
        assert!(windows_installation_match(
            "C:/Users/Example/mhc/lsp-daemon.exe",
            installation
        ));
        assert!(!windows_installation_match(
            r"C:\Users\Example\mhc-other\mhc.exe",
            installation
        ));
        assert!(windows_installation_match(
            r"\\SERVER\Share\mhc\mhc.exe",
            Path::new(r"\\?\UNC\server\share\mhc")
        ));
        assert!(!windows_installation_match(
            r"\\server\share\mhc-other\mhc.exe",
            Path::new(r"\\?\UNC\server\share\mhc")
        ));
    }
}
