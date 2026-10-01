use std::{path::Path, time::Instant};
use tokio::io::AsyncWriteExt;
use crate::{definition::AbortSignal, grep::engine::GrepEngineError};
pub async fn run(args: &[String], cwd: &Path, input: Option<Vec<u8>>, deadline: Instant, signal: &AbortSignal) -> Result<Vec<u8>, GrepEngineError> {
    if signal.is_aborted() { return Err(GrepEngineError::Aborted); }
    let mut command = tokio::process::Command::new("rg");
    command.args(args).current_dir(cwd).kill_on_drop(true).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    let mut child = command.spawn().map_err(|e| GrepEngineError::EngineUnavailable(format!("Failed to run ripgrep: {e}")))?;
    let stdin = child.stdin.take();
    let operation = async {
        let writer = async {
            if let (Some(mut stdin),Some(input)) = (stdin,input)
                && let Err(error) = stdin.write_all(&input).await
                && error.kind() != std::io::ErrorKind::BrokenPipe { return Err(GrepEngineError::EngineUnavailable(format!("Failed to write ripgrep input: {error}"))); }
            Ok(())
        };
        let reader = async { child.wait_with_output().await.map_err(|e| GrepEngineError::EngineUnavailable(e.to_string())) };
        let (_,output) = tokio::try_join!(writer,reader)?;
        let error = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if !(matches!(output.status.code(),Some(0|1)) || output.status.code() == Some(2) && error.lines().all(|line| line.starts_with("No files were searched") || line == "Running with --debug will show why files are being skipped.")) {
            let lower = error.to_lowercase();
            return Err(if lower.contains("look-around") || lower.contains("backreference") { GrepEngineError::UnsupportedRegex(error) }
                else if lower.contains("regex parse error") || lower.contains("error compiling pattern") || lower.contains("not allowed in a regex") { GrepEngineError::InvalidPattern(error) }
                else if lower.contains("error parsing glob") { GrepEngineError::InvalidGlob(error) }
                else if lower.contains("file type") { GrepEngineError::UnknownType(error) }
                else { GrepEngineError::EngineUnavailable(error) });
        } Ok(output.stdout)
    };
    tokio::select! {
        result = operation => result,
        () = signal.cancelled() => Err(GrepEngineError::Aborted),
        () = tokio::time::sleep_until(deadline.into()) => Err(GrepEngineError::EngineUnavailable("TIMED_OUT".into())),
    }
}
