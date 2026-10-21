use super::{context_manager::WorkerOptions, worker_slot::WorkerSlot};
use crate::kernels::shared::subprocess_process::ProcessError;
use maho_ai::utils::abort::{AbortController, AbortSignal};
use std::{future::Future, pin::Pin, sync::Arc};

type Startup = Pin<Box<dyn Future<Output = (WorkerSlot, Result<(), ProcessError>)> + Send>>;

/// Owns bring-up separately from the actor so commands remain admissible while readiness is pending.
pub(super) struct WorkerRecovery {
    ready: Startup,
    abort: AbortController,
}

impl WorkerRecovery {
    pub(super) fn start(
        mut slot: WorkerSlot,
        options: Arc<WorkerOptions>,
        shutdown: AbortSignal,
    ) -> Self {
        let abort = AbortController::new();
        let signal = abort.signal();
        let cancellation = abort.clone();
        let ready = Box::pin(async move {
            let result = {
                let ready = async {
                    let names = (options.names)().map_err(ProcessError::Startup)?;
                    slot.ensure_ready(options.startup(&names), &signal).await
                };
                tokio::pin!(ready);
                tokio::select! {
                    biased;
                    () = shutdown.cancelled() => {
                        cancellation.abort(shutdown.reason());
                        ready.await
                    }
                    result = &mut ready => result,
                }
            };
            (slot, result)
        });
        Self { ready, abort }
    }

    pub(super) async fn finish(&mut self) -> (WorkerSlot, Result<(), ProcessError>) {
        self.ready.as_mut().await
    }

    pub(super) async fn cancel(mut self) -> WorkerSlot {
        self.abort.abort(None);
        let (slot, _) = self.finish().await;
        slot
    }
}
