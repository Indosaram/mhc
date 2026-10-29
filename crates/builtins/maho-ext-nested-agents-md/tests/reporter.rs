use maho_ext_nested_agents_md::ui::reporter::display_path;
use std::path::Path;
#[test]
fn display_path_relativizes_paths_below_cwd() {
 assert_eq!(display_path(Path::new("/project"), Path::new("/project/src/main.rs")), "src/main.rs");
 assert_eq!(display_path(Path::new("/project/src"), Path::new("/project/src/lib.rs")), "lib.rs");
}

#[test]
fn display_path_falls_back_to_absolute_for_cwd_root_ancestor_sibling_and_relative() {
 assert_eq!(display_path(Path::new("/project"), Path::new("/project")), "/project");
 assert_eq!(display_path(Path::new("/project"), Path::new("/project/..")), "/project/..");
 assert_eq!(display_path(Path::new("/project"), Path::new("/other/src")), "/other/src");
 assert_eq!(display_path(Path::new("/project"), Path::new("relative/main.rs")), "relative/main.rs");
}

#[test]
fn theme_foreground_supports_reporter_palette_colors() {
 let theme = maho_ext_api::Theme { colors: std::collections::BTreeMap::from([("accent".into(), "<accent>".into()), ("muted".into(), "<muted>".into()), ("warning".into(), "<warning>".into())]), ..Default::default() };
 assert_eq!(theme.fg("accent", "Nested Context:"), "<accent>Nested Context:\u{1b}[39m");
 assert_eq!(theme.fg("muted", "AGENTS.md 1"), "<muted>AGENTS.md 1\u{1b}[39m");
 assert_eq!(theme.fg("warning", " ⚠ failed"), "<warning> ⚠ failed\u{1b}[39m");
}
