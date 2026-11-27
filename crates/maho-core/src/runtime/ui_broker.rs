//! Port of senpi `packages/coding-agent/src/core/runtime/ui-broker.ts`.

use std::sync::{Arc, Mutex};

use maho_ext_api::ExtensionUi;

/// The UI of a runtime that has no session (senpi#5228). One presenter at a time answers the
/// runtime's extension UI requests; with none attached, every request settles as an unbound UI does.
#[derive(Default, Clone)]
pub struct RuntimeUiBroker {
    presenter: Arc<Mutex<Option<Arc<dyn ExtensionUi>>>>,
}

pub struct PresenterDetachGuard {
    slot: Arc<Mutex<Option<Arc<dyn ExtensionUi>>>>,
    presenter_ptr: usize,
}

impl Drop for PresenterDetachGuard {
    fn drop(&mut self) {
        let mut guard = self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard
            .as_ref()
            .is_some_and(|current| Arc::as_ptr(current) as *const () as usize == self.presenter_ptr)
        {
            *guard = None;
        }
    }
}

impl RuntimeUiBroker {
    pub fn presenter(&self) -> Option<Arc<dyn ExtensionUi>> {
        self.presenter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn attach(&self, presenter: Arc<dyn ExtensionUi>) -> Result<PresenterDetachGuard, String> {
        let mut guard = self.slot_lock();
        if guard.is_some() {
            return Err("A presenter is already attached to this runtime's UI broker".to_owned());
        }
        let presenter_ptr = Arc::as_ptr(&presenter) as *const () as usize;
        *guard = Some(presenter);
        Ok(PresenterDetachGuard {
            slot: Arc::clone(&self.presenter),
            presenter_ptr,
        })
    }

    fn slot_lock(&self) -> std::sync::MutexGuard<'_, Option<Arc<dyn ExtensionUi>>> {
        self.presenter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
