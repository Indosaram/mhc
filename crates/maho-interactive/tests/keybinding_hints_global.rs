//! `keybinding-hints.ts` reads the process-wide manager (`getKeybindings()`), so a user rebind
//! installed with `setKeybindings` reaches every hint. The manager is process-global, so the swap
//! holds `global_keybindings`'s lock and is restored when the test ends.

use maho_interactive::components::keybinding_hints::{key_display_text, key_text};
use maho_tui::keybindings::{KeybindingsConfig, Keys};

#[test]
fn hints_follow_the_installed_user_keybindings() {
    let mut user = KeybindingsConfig::new();
    user.insert("app.tools.expand".into(), Some(Keys::One("ctrl+e".into())));
    let manager = maho_core::keybindings::KeybindingsManager::new(user, None);
    let _installed = crate::global_keybindings::install_global_keybindings(manager.inner().clone());
    assert_eq!(key_text("app.tools.expand"), "ctrl+e");
    assert_eq!(key_display_text("app.tools.expand"), "Ctrl+E");
}
