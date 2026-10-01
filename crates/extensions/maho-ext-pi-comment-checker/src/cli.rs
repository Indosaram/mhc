use comment_checker_core::{HookInput, ResolveCommentCheckerBinaryInput};
use std::{path::{Path, PathBuf}, process::Stdio, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

pub const MAX_PROCESS_OUTPUT_BYTES: usize = 64 * 1024;
pub const PROCESS_TIMEOUT_MS: u64 = 30_000;

#[derive(Debug, PartialEq, Eq)]
pub enum RunStatus { Pass, Warning, Error, Missing }
pub struct RunResult { pub status: RunStatus, pub message: String }

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
    let mut output = Vec::new();
    let mut chunk = [0; 8192];
    let mut truncated = false;
    loop {
        let count = stream.read(&mut chunk).await?;
        if count == 0 { break; }
        let remaining = MAX_PROCESS_OUTPUT_BYTES.saturating_sub(output.len());
        output.extend_from_slice(&chunk[..count.min(remaining)]);
        truncated |= count > remaining;
    }
    let mut text = String::from_utf8_lossy(&output).into_owned();
    if truncated {
        while text.ends_with('\u{fffd}') { text.pop(); }
        text.push_str(&format!("\n[{name} truncated after {MAX_PROCESS_OUTPUT_BYTES} bytes]"));
    }
    Ok(text)
}

pub async fn run_checker(input: &HookInput, binary: Option<&Path>) -> RunResult {
    run_checker_with_prompt(input, binary, None).await
}

pub async fn run_checker_with_prompt(input: &HookInput, binary: Option<&Path>, custom_prompt: Option<&str>) -> RunResult {
    let Some(binary) = binary else { return RunResult { status: RunStatus::Missing, message: "comment-checker binary not found. Install @code-yeongyu/comment-checker or reload the package.".into() }; };
    let mut command = tokio::process::Command::new(binary);
    command.arg("check");
    if let Some(prompt) = custom_prompt.filter(|value| !value.is_empty()) { command.args(["--prompt", prompt]); }
    let mut child = match command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true).spawn() {
        Ok(child) => child,
        Err(error) => return RunResult { status: RunStatus::Error, message: error.to_string() },
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
            let message = if stderr.is_empty() { stdout } else { stderr };
            match exit.code() {
                Some(0) => RunResult { status: RunStatus::Pass, message: String::new() },
                Some(2) => RunResult { status: RunStatus::Warning, message },
                _ => RunResult { status: RunStatus::Error, message },
            }
        }
        Ok(Err(error)) => RunResult { status: RunStatus::Error, message: error.to_string() },
        Err(_) => {
            if let Err(error) = child.kill().await { eprintln!("comment-checker cleanup: {error}"); }
            RunResult { status: RunStatus::Error, message: format!("comment-checker process timed out after {PROCESS_TIMEOUT_MS} ms") }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
