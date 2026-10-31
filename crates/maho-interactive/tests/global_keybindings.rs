//! Serializes tests that swap the process-global keybindings (senpi's `setKeybindings`), so one
//! test's bindings or restore never lands inside another's (maho-tui's `global_keybindings_lock`).

use maho_tui::keybindings::{KeybindingsManager, get_keybindings, set_keybindings};
use std::sync::{Mutex, MutexGuard, PoisonError};

static LOCK: Mutex<()> = Mutex::new(());

/// Holds the lock while installed; dropping it restores the bindings it replaced, then unlocks.
pub struct GlobalKeybindings {
    previous: KeybindingsManager,
    _lock: MutexGuard<'static, ()>,
}

impl Drop for GlobalKeybindings {
    fn drop(&mut self) {
        set_keybindings(self.previous.clone());
    }
}

/// Takes the lock and installs `manager` as the process-global keybindings.
pub fn install_global_keybindings(manager: KeybindingsManager) -> GlobalKeybindings {
    let lock = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let previous = (*get_keybindings()).clone();
    set_keybindings(manager);
    GlobalKeybindings {
        previous,
        _lock: lock,
    }
}
