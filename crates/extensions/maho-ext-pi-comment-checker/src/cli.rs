use comment_checker_core::{HookInput, ResolveCommentCheckerBinaryInput};
use std::{path::{Path, PathBuf}, process::Stdio, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

pub const MAX_PROCESS_OUTPUT_BYTES: usize = 64 * 1024;
pub const PROCESS_TIMEOUT_MS: u64 = 30_000;

#[derive(Debug, PartialEq, Eq)]
pub enum RunStatus { Pass, Warning, Error, Missing }
pub struct RunResult {
    pub status: RunStatus, pub message: String,
    pub binary_path: Option<PathBuf>, pub exit_code: Option<i32>,
    pub stdout: Option<String>, pub stderr: Option<String>,
}

pub fn resolve_binary(source_path: &Path) -> Option<PathBuf> {
    let exists = |path: &str| Path::new(path).exists();
    let binary_name = if cfg!(windows) { "comment-checker.exe" } else { "comment-checker" };
    let platform = if cfg!(windows) { "win32" } else if cfg!(target_os = "macos") { "darwin" } else { "linux" };
    let arch = if cfg!(target_arch = "x86_64") { "x64" } else if cfg!(target_arch = "aarch64") { "arm64" } else { std::env::consts::ARCH };
    for directory in source_path.parent()?.ancestors() {
        let package = directory.join("node_modules/@code-yeongyu/comment-checker");
        if package.join("package.json").exists() {
            let bundled = package.join("vendor").join(format!("{platform}-{arch}")).join(binary_name);
            if bundled.exists() { return Some(bundled); }
            break;
        }
    }
    comment_checker_core::resolve_comment_checker_binary(&ResolveCommentCheckerBinaryInput { binary_name, cached_binary_path: None, exists_sync: &exists, import_meta_url: source_path.to_str(), package_name: None }).map(PathBuf::from)
}

async fn read_output(mut stream: impl AsyncRead + Unpin, name: &str) -> std::io::Result<String> {
    let mut output = String::new();
    let mut pending = Vec::new();
    let mut chunk = [0; 8192];
    let mut truncated = false;
    loop {
        let count = stream.read(&mut chunk).await?;
        pending.extend_from_slice(&chunk[..count]);
        let mut decoded = String::new();
        let mut consumed = 0;
        while consumed < pending.len() {
            match std::str::from_utf8(&pending[consumed..]) {
                Ok(text) => { decoded.push_str(text); consumed = pending.len(); }
                Err(error) => {
                    let valid_end = consumed + error.valid_up_to();
                    decoded.push_str(std::str::from_utf8(&pending[consumed..valid_end]).expect("validated UTF-8 prefix"));
                    consumed = valid_end;
                    if let Some(length) = error.error_len() { decoded.push('\u{fffd}'); consumed += length; }
                    else if count == 0 { decoded.push('\u{fffd}'); consumed = pending.len(); }
                    else { break; }
                }
            }
        }
        pending.drain(..consumed);
        if !truncated {
            let remaining = MAX_PROCESS_OUTPUT_BYTES.saturating_sub(output.len());
            let mut end = decoded.len().min(remaining);
            while !decoded.is_char_boundary(end) { end -= 1; }
            output.push_str(&decoded[..end]);
            truncated = decoded.len() > remaining;
        }
        if count == 0 { break; }
    }
    if truncated {
        output.push_str(&format!("\n[{name} truncated after {MAX_PROCESS_OUTPUT_BYTES} bytes]"));
    }
    Ok(output)
}

pub async fn run_checker(input: &HookInput, binary: Option<&Path>) -> RunResult {
    run_checker_with_prompt(input, binary, None).await
}

pub async fn run_checker_with_prompt(input: &HookInput, binary: Option<&Path>, custom_prompt: Option<&str>) -> RunResult {
    let Some(binary) = binary else { return RunResult { status: RunStatus::Missing, message: "comment-checker binary not found. Install @code-yeongyu/comment-checker or reload the package.".into(), binary_path: None, exit_code: None, stdout: None, stderr: None }; };
    let mut command = tokio::process::Command::new(binary);
    command.arg("check");
    if let Some(prompt) = custom_prompt.filter(|value| !value.is_empty()) { command.args(["--prompt", prompt]); }
    let mut child = match command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true).spawn() {
        Ok(child) => child,
        Err(error) => return process_result(binary, None, String::new(), error.to_string()),
    };
    let operation = async {
        let mut stdin = child.stdin.take().ok_or_else(|| std::io::Error::other("comment-checker stdin missing"))?;
        let payload = serde_json::to_vec(input).map_err(std::io::Error::other)?;
        stdin.write_all(&payload).await?;
        drop(stdin);
        let stdout = child.stdout.take().ok_or_else(|| std::io::Error::other("comment-checker stdout missing"))?;
        let stderr = child.stderr.take().ok_or_else(|| std::io::Error::other("comment-checker stderr missing"))?;
        tokio::try_join!(read_output(stdout, "stdout"), read_output(stderr, "stderr"), child.wait())
    };
    match tokio::time::timeout(Duration::from_millis(PROCESS_TIMEOUT_MS), operation).await {
        Ok(Ok((stdout, stderr, exit))) => {
            process_result(binary, exit.code(), stdout, stderr)
        }
        Ok(Err(error)) => process_result(binary, None, String::new(), error.to_string()),
        Err(_) => {
            if let Err(error) = child.kill().await { eprintln!("comment-checker cleanup: {error}"); }
            process_result(binary, None, String::new(), format!("comment-checker process timed out after {PROCESS_TIMEOUT_MS} ms"))
        }
    }
}

fn process_result(binary: &Path, exit_code: Option<i32>, stdout: String, stderr: String) -> RunResult {
    let status = match exit_code { Some(0) => RunStatus::Pass, Some(2) => RunStatus::Warning, _ => RunStatus::Error };
    let message = if status == RunStatus::Pass { String::new() } else if stderr.is_empty() { stdout.clone() } else { stderr.clone() };
    RunResult { status, message, binary_path: Some(binary.to_owned()), exit_code, stdout: Some(stdout), stderr: Some(stderr) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pass_preserves_process_output() {
        let result = process_result(Path::new("checker"), Some(0), "output".into(), "diagnostic".into());
        assert_eq!(result.status, RunStatus::Pass);
        assert!(result.message.is_empty());
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.stdout.as_deref(), Some("output"));
        assert_eq!(result.stderr.as_deref(), Some("diagnostic"));
    }
    #[test]
    fn warning_prefers_stderr() {
        let result = process_result(Path::new("checker"), Some(2), "stdout".into(), "stderr".into());
        assert_eq!(result.status, RunStatus::Warning);
        assert_eq!(result.message, "stderr");
        assert_eq!(result.binary_path, Some(PathBuf::from("checker")));
    }
    #[test]
    fn error_uses_stdout_when_stderr_empty() {
        let result = process_result(Path::new("checker"), Some(1), "failure".into(), String::new());
        assert_eq!(result.status, RunStatus::Error);
        assert_eq!(result.message, "failure");
    }
    #[tokio::test]
    async fn valid_replacement_character_is_not_removed_at_limit() {
        let mut bytes = vec![b'x'; MAX_PROCESS_OUTPUT_BYTES - 3];
        bytes.extend_from_slice("\u{fffd}extra".as_bytes());
        let output = read_output(bytes.as_slice(), "stdout").await.expect("read replacement fixture");
        assert!(output.contains("\u{fffd}\n[stdout truncated"));
    }
    #[tokio::test]
    async fn malformed_utf8_counts_decoded_bytes() {
        let bytes = vec![0xff; MAX_PROCESS_OUTPUT_BYTES];
        let output = read_output(bytes.as_slice(), "stdout").await.expect("decode malformed output");
        assert_eq!(output.split('\n').next().expect("output prefix").len(), MAX_PROCESS_OUTPUT_BYTES / 3 * 3);
        assert!(output.contains("[stdout truncated"));
    }

    #[test]
    fn bundled_when_package_has_vendor_binary() {
        let directory = tempfile::tempdir().expect("create package fixture");
        let package = directory.path().join("node_modules/@code-yeongyu/comment-checker");
        let platform = if cfg!(windows) { "win32" } else if cfg!(target_os = "macos") { "darwin" } else { "linux" };
        let arch = if cfg!(target_arch = "x86_64") { "x64" } else if cfg!(target_arch = "aarch64") { "arm64" } else { std::env::consts::ARCH };
        let name = if cfg!(windows) { "comment-checker.exe" } else { "comment-checker" };
        let bundled = package.join("vendor").join(format!("{platform}-{arch}")).join(name);
        std::fs::create_dir_all(bundled.parent().expect("vendor parent")).expect("create vendor directory");
        std::fs::create_dir_all(package.join("bin")).expect("create fallback directory");
        std::fs::write(package.join("package.json"), "{}").expect("write package metadata");
        std::fs::write(&bundled, "binary").expect("write bundled fixture");
        std::fs::write(package.join("bin").join(name), "fallback").expect("write fallback fixture");
        assert_eq!(resolve_binary(&directory.path().join("extension.rs")), Some(bundled));
    }

    #[tokio::test]
    async fn bounded_when_multibyte_output_crosses_limit() {
        let mut bytes = vec![b'x'; MAX_PROCESS_OUTPUT_BYTES - 1];
        bytes.extend_from_slice("é".as_bytes());
        let output = read_output(bytes.as_slice(), "stderr").await.expect("read fixture output");
        assert_eq!(output, format!("{}\n[stderr truncated after {MAX_PROCESS_OUTPUT_BYTES} bytes]", "x".repeat(MAX_PROCESS_OUTPUT_BYTES - 1)));
    }
}
