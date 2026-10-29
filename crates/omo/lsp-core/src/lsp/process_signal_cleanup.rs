use crate::lsp::cleanup_errors::report_best_effort_cleanup_error;
use std::future::Future;
use tokio::sync::oneshot;

/// TS `installProcessSignalCleanup`: runs `cleanup` on SIGINT/SIGTERM (plus Ctrl-Break on
/// Windows). Dropping the returned guard is the TS disposer. As with a Node listener, the
/// signal's default termination is replaced while the process runs.
pub fn install_process_signal_cleanup<F, Fut>(cleanup: F) -> Option<SignalCleanupGuard>
where
    F: Fn() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let handle = tokio::runtime::Handle::try_current().ok()?;
    let _entered = handle.enter();
    let mut signals = match ProcessSignals::register() {
        Ok(signals) => signals,
        Err(error) => {
            report_best_effort_cleanup_error("signal cleanup", &error.to_string());
            return None;
        }
    };
    let (stop, mut stopped) = oneshot::channel::<()>();
    handle.spawn(async move {
        loop {
            tokio::select! {
                _ = &mut stopped => return,
                received = signals.next() => {
                    if !received {
                        return;
                    }
                }
            }
            cleanup().await;
        }
    });
    Some(SignalCleanupGuard { _stop: stop })
}

#[derive(Debug)]
pub struct SignalCleanupGuard {
    _stop: oneshot::Sender<()>,
}

#[cfg(unix)]
struct ProcessSignals {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl ProcessSignals {
    fn register() -> std::io::Result<Self> {
        use tokio::signal::unix::SignalKind;
        use tokio::signal::unix::signal;
        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
        })
    }

    async fn next(&mut self) -> bool {
        tokio::select! {
            received = self.interrupt.recv() => received.is_some(),
            received = self.terminate.recv() => received.is_some(),
        }
    }
}

#[cfg(windows)]
struct ProcessSignals {
    interrupt: tokio::signal::windows::CtrlC,
    brk: tokio::signal::windows::CtrlBreak,
}

#[cfg(windows)]
impl ProcessSignals {
    fn register() -> std::io::Result<Self> {
        Ok(Self {
            interrupt: tokio::signal::windows::ctrl_c()?,
            brk: tokio::signal::windows::ctrl_break()?,
        })
    }

    async fn next(&mut self) -> bool {
        tokio::select! {
            received = self.interrupt.recv() => received.is_some(),
            received = self.brk.recv() => received.is_some(),
        }
    }
}

#[cfg(all(test, unix))]
#[path = "process_signal_cleanup_tests.rs"]
mod tests;
