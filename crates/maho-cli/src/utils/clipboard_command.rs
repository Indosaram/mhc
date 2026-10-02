use std::process::Stdio;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[derive(Default)]
pub struct ClipboardCommandOptions<'a> { pub input: Option<&'a str>, pub timeout_ms: Option<u64>, pub max_buffer_bytes: Option<usize> }
pub async fn run_clipboard_command(command: &str, args: &[&str], options: ClipboardCommandOptions<'_>) -> Option<Vec<u8>> {
    let mut child = tokio::process::Command::new(command).args(args).stdin(Stdio::piped()).stdout(if options.input.is_some() { Stdio::null() } else { Stdio::piped() }).stderr(Stdio::null()).kill_on_drop(true).spawn().ok()?;
    let mut stdin = child.stdin.take()?; let mut stdout = child.stdout.take();
    let input = options.input; let maximum = options.max_buffer_bytes.unwrap_or(50 * 1024 * 1024);
    let operation = async {
        let write = async { if let Some(input) = input { let _ = stdin.write_all(input.as_bytes()).await; } drop(stdin); };
        let read = async { let mut bytes = Vec::new(); if let Some(stdout) = stdout.as_mut() { stdout.take(maximum as u64 + 1).read_to_end(&mut bytes).await.ok()?; if bytes.len() > maximum { return None; } } Some(bytes) };
        let ((), bytes) = tokio::join!(write, read); let bytes = bytes?; child.wait().await.ok()?.success().then_some(bytes)
    };
    tokio::time::timeout(std::time::Duration::from_millis(options.timeout_ms.unwrap_or(3000)), operation).await.ok().flatten()
}
