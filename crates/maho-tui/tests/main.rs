//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "autocomplete_slash_dollar_tests.rs"]
pub mod autocomplete_slash_dollar_tests;
#[path = "editor_marker_tests.rs"]
pub mod editor_marker_tests;
#[path = "editor_tests.rs"]
pub mod editor_tests;
#[path = "layout_tests.rs"]
pub mod layout_tests;
#[path = "markdown_golden.rs"]
pub mod markdown_golden;
#[path = "mouse_input_tests.rs"]
pub mod mouse_input_tests;
#[path = "tui_lifecycle_tests.rs"]
pub mod tui_lifecycle_tests;
#[path = "tui_screen_golden.rs"]
pub mod tui_screen_golden;
#[path = "width_golden.rs"]
pub mod width_golden;

// Helpers several test modules share, declared once so each file compiles once.
#[path = "common/mod.rs"]
pub mod common;
