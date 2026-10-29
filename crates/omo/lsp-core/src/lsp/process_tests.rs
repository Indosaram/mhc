use super::*;
use pretty_assertions::assert_eq;

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

#[test]
fn validate_cwd_reports_missing_and_non_directory_paths() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("file.txt");
    std::fs::write(&file, "x").expect("write");
    let missing = dir.path().join("missing");

    assert_eq!(
        validate_cwd(&dir.path().to_string_lossy()),
        CwdValidation {
            valid: true,
            error: None
        }
    );
    assert_eq!(
        validate_cwd(&missing.to_string_lossy()),
        CwdValidation {
            valid: false,
            error: Some(format!(
                "Working directory does not exist: {}",
                missing.display()
            )),
        }
    );
    assert_eq!(
        validate_cwd(&file.to_string_lossy()),
        CwdValidation {
            valid: false,
            error: Some(format!("Path is not a directory: {}", file.display())),
        }
    );
}

#[test]
fn create_spawn_command_passes_unix_commands_through_and_wraps_windows_shims() {
    let env = BTreeMap::new();
    assert_eq!(
        create_spawn_command(
            &strings(&["tsserver", "--stdio"]),
            SpawnPlatform::Unix,
            "cmd.exe",
            &env
        ),
        Ok(PreparedSpawnCommand {
            command: "tsserver".to_string(),
            args: strings(&["--stdio"]),
            shell: false,
        })
    );
    assert_eq!(
        create_spawn_command(
            &strings(&["tool.cmd", "--stdio"]),
            SpawnPlatform::Windows,
            "cmd.exe",
            &env
        ),
        Ok(PreparedSpawnCommand {
            command: "cmd.exe".to_string(),
            args: strings(&["/d", "/s", "/c", "tool.cmd", "--stdio"]),
            shell: false,
        })
    );
    assert_eq!(
        create_spawn_command(&[], SpawnPlatform::Unix, "cmd.exe", &env),
        Err(LspError::ProcessSpawn("[lsp] empty command".to_string()))
    );
}

#[tokio::test]
async fn spawn_process_rejects_invalid_cwd() {
    let error = spawn_process(
        &strings(&["true"]),
        &SpawnOptions {
            cwd: "/definitely/not/a/dir".to_string(),
            env: BTreeMap::new(),
        },
    )
    .expect_err("invalid cwd");
    assert_eq!(
        error,
        LspError::InvalidPath(
            "[lsp] Working directory does not exist: /definitely/not/a/dir".to_string()
        )
    );
}
