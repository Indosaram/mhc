use maho_cli::config::resolve_shipped_asset_dir;
#[test]
fn asset_override_requires_probe_and_falls_back_to_installation() {
    let root = tempfile::tempdir().unwrap();
    let preferred = root.path().join("preferred"); let installed = root.path().join("installed");
    std::fs::create_dir_all(preferred.join("theme")).unwrap();
    assert_eq!(resolve_shipped_asset_dir(&preferred, &installed, "theme", "dark.json"), installed.join("theme"));
    std::fs::write(preferred.join("theme/dark.json"), "{}").unwrap();
    assert_eq!(resolve_shipped_asset_dir(&preferred, &installed, "theme", "dark.json"), preferred.join("theme"));
}
