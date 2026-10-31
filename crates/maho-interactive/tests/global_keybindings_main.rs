//! Test binary for the tests that swap the process-global keybindings (senpi `setKeybindings`). They
//! run in their own process, away from the integration binary's unlocked writers and readers of that
//! global, and serialize among themselves through `global_keybindings`'s lock.

#[path = "global_keybindings.rs"]
mod global_keybindings;
#[path = "keybinding_hints_global.rs"]
mod keybinding_hints_global;
#[path = "task34.rs"]
mod task34;
