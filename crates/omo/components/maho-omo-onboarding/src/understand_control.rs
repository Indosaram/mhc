//! Skip and stop for one understanding run (upstream `understand-control.ts`); the RPC shell holds it, the
//! runtime reads it between and during steps.

use std::{
    collections::HashSet,
    sync::{Arc, Mutex, PoisonError},
};

pub type Cancel = Arc<dyn Fn() + Send + Sync>;

struct Running {
    projects: HashSet<String>,
    cancel: Cancel,
}

#[derive(Default)]
struct Inner {
    stop_requested: bool,
    skipped: HashSet<String>,
    running: Option<Running>,
}

#[derive(Default)]
pub struct OnboardingUnderstandingControl {
    inner: Mutex<Inner>,
}

impl OnboardingUnderstandingControl {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn stopped(&self) -> bool {
        self.lock().stop_requested
    }

    pub fn is_skipped(&self, project: &str) -> bool {
        self.lock().skipped.contains(project)
    }

    /// Cancels the running step when its project set contains `project`.
    pub fn skip(&self, project: &str) {
        let cancel = {
            let mut inner = self.lock();
            inner.skipped.insert(project.to_owned());
            inner
                .running
                .as_ref()
                .filter(|running| running.projects.contains(project))
                .map(|running| Arc::clone(&running.cancel))
        };
        if let Some(cancel) = cancel {
            cancel();
        }
    }

    pub fn stop(&self) {
        let cancel = {
            let mut inner = self.lock();
            inner.stop_requested = true;
            inner
                .running
                .as_ref()
                .map(|running| Arc::clone(&running.cancel))
        };
        if let Some(cancel) = cancel {
            cancel();
        }
    }

    /// Cancels immediately when the run is already stopped or one of its projects was skipped.
    pub fn attach(&self, projects: HashSet<String>, cancel: Cancel) {
        let now = {
            let mut inner = self.lock();
            let now = inner.stop_requested
                || projects
                    .iter()
                    .any(|project| inner.skipped.contains(project));
            inner.running = Some(Running {
                projects,
                cancel: Arc::clone(&cancel),
            });
            now
        };
        if now {
            cancel();
        }
    }

    pub fn detach(&self) {
        self.lock().running = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn counter() -> (Arc<AtomicUsize>, Cancel) {
        let count = Arc::new(AtomicUsize::new(0));
        let cancel_count = Arc::clone(&count);
        (
            count,
            Arc::new(move || {
                cancel_count.fetch_add(1, Ordering::SeqCst);
            }),
        )
    }

    #[test]
    fn skip_cancels_only_a_running_step_that_holds_the_project() {
        let control = OnboardingUnderstandingControl::new();
        let (count, cancel) = counter();
        control.attach(["/a".to_owned()].into_iter().collect(), cancel);
        control.skip("/b");
        assert_eq!(count.load(Ordering::SeqCst), 0);
        control.skip("/a");
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(control.is_skipped("/a") && !control.stopped());
    }

    #[test]
    fn attach_cancels_immediately_after_stop_or_skip() {
        let control = OnboardingUnderstandingControl::new();
        control.skip("/a");
        let (count, cancel) = counter();
        control.attach(["/a".to_owned()].into_iter().collect(), cancel);
        assert_eq!(count.load(Ordering::SeqCst), 1);
        control.detach();
        control.stop();
        assert!(control.stopped());
        let (count, cancel) = counter();
        control.attach(["/z".to_owned()].into_iter().collect(), cancel);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}
