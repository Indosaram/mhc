//! Session-scoped residency wake signal, lazily armed and retired when notified.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

struct ResidencyWaiter {
    fired: Mutex<bool>,
    changed: Condvar,
}

impl ResidencyWaiter {
    fn new() -> Self {
        Self {
            fired: Mutex::new(false),
            changed: Condvar::new(),
        }
    }
}

#[derive(Default)]
pub struct ResidencySignal {
    waiters: Mutex<HashMap<String, Arc<ResidencyWaiter>>>,
}

impl ResidencySignal {
    pub fn changed(&self, parent_session_id: &str) -> ResidencyChange {
        let mut waiters = self.waiters.lock().unwrap_or_else(PoisonError::into_inner);
        let waiter = Arc::clone(
            waiters
                .entry(parent_session_id.to_string())
                .or_insert_with(|| Arc::new(ResidencyWaiter::new())),
        );
        ResidencyChange { waiter }
    }

    pub fn notify(&self, parent_session_id: Option<&str>) {
        let armed = {
            let mut waiters = self.waiters.lock().unwrap_or_else(PoisonError::into_inner);
            match parent_session_id {
                Some(session_id) => waiters.remove(session_id).into_iter().collect::<Vec<_>>(),
                None => waiters.drain().map(|(_, waiter)| waiter).collect(),
            }
        };
        for waiter in armed {
            *waiter.fired.lock().unwrap_or_else(PoisonError::into_inner) = true;
            waiter.changed.notify_all();
        }
    }
}

#[derive(Clone)]
pub struct ResidencyChange {
    waiter: Arc<ResidencyWaiter>,
}

impl ResidencyChange {
    pub fn wait(&self) {
        let mut fired = self
            .waiter
            .fired
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        while !*fired {
            fired = self
                .waiter
                .changed
                .wait(fired)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    pub fn wait_timeout(&self, timeout: Duration) -> bool {
        let fired = self
            .waiter
            .fired
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if *fired {
            return true;
        }
        let (fired, _) = self
            .waiter
            .changed
            .wait_timeout_while(fired, timeout, |fired| !*fired)
            .unwrap_or_else(PoisonError::into_inner);
        *fired
    }

    pub fn is_fired(&self) -> bool {
        *self
            .waiter
            .fired
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::ResidencySignal;
    use std::time::Duration;

    #[test]
    fn given_repeated_changed_before_notify_when_armed_then_waiter_is_shared() {
        let signal = ResidencySignal::default();
        let first = signal.changed("parent");
        let second = signal.changed("parent");
        signal.notify(Some("parent"));
        assert!(first.is_fired());
        assert!(second.is_fired());
    }

    #[test]
    fn given_two_sessions_when_one_is_notified_then_only_its_waiter_fires() {
        let signal = ResidencySignal::default();
        let first = signal.changed("first");
        let second = signal.changed("second");
        signal.notify(Some("first"));
        assert!(first.wait_timeout(Duration::ZERO));
        assert!(!second.is_fired());
        assert!(!second.wait_timeout(Duration::ZERO));
    }

    #[test]
    fn given_multiple_sessions_when_unknown_session_notifies_then_all_waiters_fire() {
        let signal = ResidencySignal::default();
        let first = signal.changed("first");
        let second = signal.changed("second");
        signal.notify(None);
        assert!(first.is_fired());
        assert!(second.wait_timeout(Duration::ZERO));
    }

    #[test]
    fn given_notified_session_when_changed_again_then_waiter_is_fresh() {
        let signal = ResidencySignal::default();
        let previous = signal.changed("parent");
        signal.notify(Some("parent"));
        let fresh = signal.changed("parent");
        assert!(previous.is_fired());
        assert!(!fresh.is_fired());
        assert!(!fresh.wait_timeout(Duration::ZERO));
    }
}
