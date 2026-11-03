//! Port of senpi `packages/coding-agent/src/core/reload-veto.ts`.

use maho_ext_api::{EventKind, EventResult, ExtensionEvent, ExtensionFailure, ReloadVetoDecision};

/// `Pick<ExtensionRunner, "hasHandlers" | "emit">`.
#[async_trait::async_trait(?Send)]
pub trait ReloadVetoRunner {
    fn has_handlers(&self, kind: EventKind) -> bool;
    async fn emit(&mut self, event: ExtensionEvent) -> Result<EventResult, ExtensionFailure>;
}

#[async_trait::async_trait(?Send)]
impl ReloadVetoRunner for maho_ext_host::ExtensionRunner {
    fn has_handlers(&self, kind: EventKind) -> bool {
        maho_ext_host::ExtensionRunner::has_handlers(self, kind)
    }

    async fn emit(&mut self, event: ExtensionEvent) -> Result<EventResult, ExtensionFailure> {
        maho_ext_host::ExtensionRunner::emit(self, event).await
    }
}

fn cancelled(reason: &str) -> ReloadVetoDecision {
    ReloadVetoDecision {
        cancelled: true,
        reason: Some(reason.to_owned()),
    }
}

fn admission_veto(
    is_prompt_start_pending: &dyn Fn() -> bool,
    is_session_start_dispatching: &dyn Fn() -> bool,
) -> Option<ReloadVetoDecision> {
    if is_prompt_start_pending() {
        return Some(cancelled("A prompt is being admitted."));
    }
    if is_session_start_dispatching() {
        return Some(cancelled("A session is starting."));
    }
    None
}

/// Whether a reload must wait: a prompt being admitted or a session starting vetoes it before and after
/// the `session_before_reload` handlers run, and a handler may cancel it.
pub async fn check_session_reload_veto(
    runner: &mut dyn ReloadVetoRunner,
    is_prompt_start_pending: &dyn Fn() -> bool,
    is_session_start_dispatching: &dyn Fn() -> bool,
) -> Result<ReloadVetoDecision, ExtensionFailure> {
    if let Some(veto) = admission_veto(is_prompt_start_pending, is_session_start_dispatching) {
        return Ok(veto);
    }
    let result = if runner.has_handlers(EventKind::SessionBeforeReload) {
        Some(runner.emit(ExtensionEvent::SessionBeforeReload).await?)
    } else {
        None
    };
    if let Some(veto) = admission_veto(is_prompt_start_pending, is_session_start_dispatching) {
        return Ok(veto);
    }
    match result {
        Some(EventResult::SessionBefore(before)) if before.cancel == Some(true) => {
            Ok(ReloadVetoDecision {
                cancelled: true,
                reason: before.reason,
            })
        }
        _ => Ok(ReloadVetoDecision::default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ext_api::SessionBeforeEventResult;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct FakeRunner {
        handlers: bool,
        result: fn() -> EventResult,
        emitted: usize,
        on_emit: Option<&'static AtomicBool>,
    }

    #[async_trait::async_trait(?Send)]
    impl ReloadVetoRunner for FakeRunner {
        fn has_handlers(&self, kind: EventKind) -> bool {
            self.handlers && kind == EventKind::SessionBeforeReload
        }

        async fn emit(&mut self, _event: ExtensionEvent) -> Result<EventResult, ExtensionFailure> {
            self.emitted += 1;
            if let Some(flag) = self.on_emit {
                flag.store(true, Ordering::SeqCst);
            }
            Ok((self.result)())
        }
    }

    fn runner(handlers: bool, result: fn() -> EventResult) -> FakeRunner {
        FakeRunner {
            handlers,
            result,
            emitted: 0,
            on_emit: None,
        }
    }

    #[tokio::test]
    async fn pending_admission_vetoes_before_handlers_run() {
        let mut fake = runner(true, || EventResult::None);
        let decision = check_session_reload_veto(&mut fake, &|| true, &|| false).await;
        assert_eq!(
            decision.ok(),
            Some(cancelled("A prompt is being admitted."))
        );
        assert_eq!(fake.emitted, 0);
        let decision = check_session_reload_veto(&mut fake, &|| false, &|| true).await;
        assert_eq!(decision.ok(), Some(cancelled("A session is starting.")));
    }

    #[tokio::test]
    async fn admission_started_during_handlers_still_vetoes() {
        static STARTED: AtomicBool = AtomicBool::new(false);
        let mut fake = FakeRunner {
            on_emit: Some(&STARTED),
            ..runner(true, || EventResult::None)
        };
        let pending = || STARTED.load(Ordering::SeqCst);
        let decision = check_session_reload_veto(&mut fake, &pending, &|| false).await;
        assert_eq!(
            decision.ok(),
            Some(cancelled("A prompt is being admitted."))
        );
    }

    #[tokio::test]
    async fn a_handler_cancel_carries_its_reason() {
        let mut fake = runner(true, || {
            EventResult::SessionBefore(SessionBeforeEventResult {
                cancel: Some(true),
                reason: Some("busy".into()),
                ..Default::default()
            })
        });
        let decision = check_session_reload_veto(&mut fake, &|| false, &|| false).await;
        assert_eq!(decision.ok(), Some(cancelled("busy")));

        let mut quiet = runner(false, || EventResult::None);
        let decision = check_session_reload_veto(&mut quiet, &|| false, &|| false).await;
        assert_eq!(decision.ok(), Some(ReloadVetoDecision::default()));
        assert_eq!(quiet.emitted, 0);
    }
}
