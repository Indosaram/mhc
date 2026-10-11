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
