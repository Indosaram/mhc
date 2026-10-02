use maho_ai::utils::abort::AbortSignal;
use tokio::io::AsyncReadExt;
pub struct WaitForChildProcessOptions<'a> { pub signal: Option<&'a AbortSignal>, pub abort_exit_grace_ms: u64 }
impl Default for WaitForChildProcessOptions<'_> { fn default() -> Self { Self { signal: None, abort_exit_grace_ms: 5000 } } }
pub async fn wait_for_child_process(child: &mut tokio::process::Child, options: WaitForChildProcessOptions<'_>, mut on_output: impl FnMut(bool, &[u8])) -> std::io::Result<Option<i32>> {
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let mut out = [0u8; 8192];
    let mut err = [0u8; 8192];
    let mut exit: Option<Option<i32>> = None;
    let mut idle = None;
    let mut abort_deadline = None;
    let mut aborted = false;
    loop {
        if stdout.is_none() && stderr.is_none() && let Some(code) = exit { return Ok(code); }
        tokio::select! {
            _ = async { if let Some(signal) = options.signal { signal.cancelled().await } else { std::future::pending().await } }, if !aborted => {
                aborted = true;
                stdout = None; stderr = None;
                if let Some(code) = exit { return Ok(code); }
                abort_deadline = Some(tokio::time::Instant::now() + std::time::Duration::from_millis(options.abort_exit_grace_ms));
            }
            status = child.wait(), if exit.is_none() => {
                exit = Some(status?.code());
                idle = Some(tokio::time::Instant::now() + std::time::Duration::from_millis(250));
            }
            read = async { stdout.as_mut().expect("stdout present").read(&mut out).await }, if stdout.is_some() => {
                let count = read?;
                if count == 0 { stdout = None; } else { on_output(false, &out[..count]); if exit.is_some() { idle = Some(tokio::time::Instant::now() + std::time::Duration::from_millis(250)); } }
            }
            read = async { stderr.as_mut().expect("stderr present").read(&mut err).await }, if stderr.is_some() => {
                let count = read?;
                if count == 0 { stderr = None; } else { on_output(true, &err[..count]); if exit.is_some() { idle = Some(tokio::time::Instant::now() + std::time::Duration::from_millis(250)); } }
            }
            _ = async { if let Some(deadline) = idle { tokio::time::sleep_until(deadline).await } else { std::future::pending().await } } => return Ok(exit.flatten()),
            _ = async { if let Some(deadline) = abort_deadline { tokio::time::sleep_until(deadline).await } else { std::future::pending().await } } => return Ok(exit.flatten()),
        }
    }
}
