//! Rust port of senpi `@earendil-works/pi-tui` (packages/tui/src).
//!
//! Module ownership follows the plan: todo 6 (terminal layer), 7 (renderer and base components),
//! 8 (editor and autocomplete), 9 (rich content and tmux image probing).

pub mod alt_screen_search; // todo 7
pub mod autocomplete; // todo 8
pub mod components; // todo 7-9
pub mod dollar_invocation_autocomplete; // todo 8
pub mod editor_component; // todo 8
pub mod fuzzy; // todo 6
pub mod image_markers; // todo 8
pub mod image_stub; // todo 7 (minimal shim; todo 9 replaces with the full terminal-image.ts port)
pub mod keybindings; // todo 6
pub mod keys; // todo 6
pub mod kill_ring; // todo 8
pub mod layout; // todo 7
pub mod layout_node; // todo 7
pub mod mouse_input; // todo 7
pub mod mux; // todo 6
pub mod native_modifiers; // todo 6
pub mod native_module_path; // todo 6
pub mod native_platform; // todo 6
pub mod paste_markers; // todo 8
pub mod process_env; // todo 6
pub mod process_stdio; // todo 6
pub mod slash_command_autocomplete; // todo 8
pub mod stderr_observer; // todo 6
pub mod stdin_buffer; // todo 6
pub mod terminal; // todo 6
pub mod terminal_capabilities; // todo 6
pub mod terminal_colors; // todo 6
pub mod terminal_image; // todo 9
pub mod terminal_text; // todo 6
pub mod tmux_cursor_query; // todo 6
pub mod tmux_focus; // todo 6
pub mod tmux_image_capability; // todo 9
pub mod tmux_image_probe; // todo 9
pub mod tui; // todo 7
pub mod tui_alt_screen; // todo 7
pub mod tui_main_screen; // todo 7
pub mod undo_stack; // todo 8
pub mod unicode_tables; // todo 6
pub mod utils; // todo 6
pub mod word_navigation; // todo 6
