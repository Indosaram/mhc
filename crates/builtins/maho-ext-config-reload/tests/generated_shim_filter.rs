use maho_ext_config_reload::generated_shim_filter::exclude_generated_extension_shims;
use std::fs;

#[test]
fn generated_global_shim_is_dropped() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("extensions")).unwrap();
    let shim = root.path().join("extensions/diff.js");
    fs::write(&shim, maho_core::generated_shim_banner::generated_shim_banner()).unwrap();
    assert!(exclude_generated_extension_shims(&[shim], root.path()).is_empty());
}
#[test]
fn user_extension_passes_through() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("extensions")).unwrap();
    let real = root.path().join("extensions/mine.js");
    fs::write(&real, "export default () => {};\n").unwrap();
    assert_eq!(exclude_generated_extension_shims(std::slice::from_ref(&real), root.path()), [real]);
}
#[test]
fn replaced_shim_is_watched_again() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("extensions")).unwrap();
    let replaced = root.path().join("extensions/tps.js");
    fs::write(&replaced, "export default () => {};\n").unwrap();
    assert_eq!(exclude_generated_extension_shims(std::slice::from_ref(&replaced), root.path()), [replaced]);
}
#[test]
fn paths_outside_extensions_are_never_dropped() {
    let root = tempfile::tempdir().unwrap();
    let settings = root.path().join("settings.json");
    fs::write(&settings, maho_core::generated_shim_banner::generated_shim_banner()).unwrap();
    assert_eq!(exclude_generated_extension_shims(std::slice::from_ref(&settings), root.path()), [settings]);
}
