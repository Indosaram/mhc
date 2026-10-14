//! `keybinding-hints.ts` reads the process-wide manager (`getKeybindings()`), so a user rebind
//! installed with `setKeybindings` reaches every hint. Kept in its own test binary because the
//! manager is process-global.

use maho_interactive::components::keybinding_hints::{key_display_text, key_text};
use maho_tui::keybindings::{KeybindingsConfig, Keys};

#[test]
fn hints_follow_the_installed_user_keybindings() {
    let mut user = KeybindingsConfig::new();
    user.insert("app.tools.expand".into(), Some(Keys::One("ctrl+e".into())));
    let manager = maho_core::keybindings::KeybindingsManager::new(user, None);
    maho_tui::keybindings::set_keybindings(manager.inner().clone());
    assert_eq!(key_text("app.tools.expand"), "ctrl+e");
    assert_eq!(key_display_text("app.tools.expand"), "Ctrl+E");
}
