pub mod detection;
pub mod highlight;
pub mod registry;
pub mod reserved_keyword_rainbow; // ledger sync-5152
pub mod system_theme; // ledger sync-5152
pub mod terminal_theme_cache;
pub use highlight::{
    get_language_from_path, highlight_code, load_all_highlight_languages, supports_language,
};
#[path = "theme.rs"]
pub mod palette;
pub use palette as theme;
pub mod theme_controller;
pub mod theme_json;
pub use theme::{
    ColorMode, TerminalTheme, Theme, ThemeBg, ThemeColor, detect_color_mode,
    parse_auto_theme_setting, resolve_theme_setting,
};
