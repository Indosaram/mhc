//! Port of senpi packages/ai/src/utils/abort-signals.ts.

use super::abort::{AbortController, AbortSignal, ListenerId};

pub struct CombinedAbortSignal {
    pub signal: Option<AbortSignal>,
    listeners: Vec<(AbortSignal, ListenerId)>,
}

impl CombinedAbortSignal {
    pub fn cleanup(&self) {
        for (signal, id) in &self.listeners {
            signal.remove_abort_listener(*id);
        }
    }
}

pub fn combine_abort_signals(signals: &[Option<AbortSignal>]) -> CombinedAbortSignal {
    let active: Vec<&AbortSignal> = signals.iter().flatten().collect();
    match active.as_slice() {
        [] => return CombinedAbortSignal { signal: None, listeners: Vec::new() },
        [only] => return CombinedAbortSignal { signal: Some((*only).clone()), listeners: Vec::new() },
        _ => {}
    }

    let controller = AbortController::new();
    let mut listeners = Vec::new();
    for signal in active {
        if let Some(reason) = signal.reason() {
            controller.abort(Some(reason));
            break;
        }
        let forward = controller.clone();
        let id = signal.add_abort_listener(move |reason| forward.abort(Some(reason.clone())));
        listeners.push((signal.clone(), id));
    }

    CombinedAbortSignal { signal: Some(controller.signal()), listeners }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::abort::AbortReason;

    #[test]
    fn empty_and_single_inputs_pass_through() {
        assert!(combine_abort_signals(&[None, None]).signal.is_none());
        let a = AbortController::new();
        let combined = combine_abort_signals(&[None, Some(a.signal())]);
        a.abort(None);
        assert!(combined.signal.as_ref().is_some_and(AbortSignal::aborted));
    }

    #[test]
    fn combined_signal_aborts_with_first_reason_and_cleanup_detaches() {
        let a = AbortController::new();
        let b = AbortController::new();
        let combined = combine_abort_signals(&[Some(a.signal()), Some(b.signal())]);
        let signal = combined.signal.clone().expect("signal");
        b.abort(Some(AbortReason::new("Error", "b")));
        assert_eq!(signal.reason(), Some(AbortReason::new("Error", "b")));

        let c = AbortController::new();
        let d = AbortController::new();
        let second = combine_abort_signals(&[Some(c.signal()), Some(d.signal())]);
        second.cleanup();
        c.abort(None);
        assert!(!second.signal.as_ref().is_some_and(AbortSignal::aborted));
    }

    #[test]
    fn already_aborted_input_aborts_combined_immediately() {
        let a = AbortController::new();
        a.abort(Some(AbortReason::new("Error", "early")));
        let b = AbortController::new();
        let combined = combine_abort_signals(&[Some(a.signal()), Some(b.signal())]);
        assert_eq!(combined.signal.and_then(|s| s.reason()), Some(AbortReason::new("Error", "early")));
    }
}
