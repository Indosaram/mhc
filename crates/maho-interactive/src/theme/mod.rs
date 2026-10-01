pub mod detection;
pub mod highlight;
pub mod registry;
pub mod terminal_theme_cache;
pub use highlight::{get_language_from_path, highlight_code};
#[path = "theme.rs"]
pub mod palette;
pub use palette as theme;
pub mod theme_controller;
pub mod theme_json;
pub use theme::{
    ColorMode, TerminalTheme, Theme, ThemeBg, ThemeColor, parse_auto_theme_setting,
    resolve_theme_setting,
};
