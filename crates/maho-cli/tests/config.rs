use maho_cli::config::resolve_shipped_asset_dir;
#[test]
fn asset_override_requires_probe_and_falls_back_to_installation() {
    let root = tempfile::tempdir().unwrap();
    let preferred = root.path().join("preferred");
    let installed = root.path().join("installed");
    std::fs::create_dir_all(preferred.join("theme")).unwrap();
    assert_eq!(
        resolve_shipped_asset_dir(&preferred, &installed, "theme", "dark.json"),
        installed.join("theme")
    );
    std::fs::write(preferred.join("theme/dark.json"), "{}").unwrap();
    assert_eq!(
        resolve_shipped_asset_dir(&preferred, &installed, "theme", "dark.json"),
        preferred.join("theme")
    );
}

// senpi config.test.ts "shipped asset directories": a foreign PACKAGE_DIR root that does not ship an
// asset answers with the running install; one that ships it wins. The running install is a native
// binary layout built from the repository's real asset files.
#[test]
fn upstream_shipped_asset_directories_prefer_a_root_that_ships_them() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let sources = [
        (
            "theme",
            workspace.join("crates/maho-interactive/src/theme/dark.json"),
        ),
        (
            "export-html",
            workspace.join("crates/maho-core/src/export_html/templates/template.html"),
        ),
        (
            "assets",
            workspace.join("crates/maho-interactive/assets/clankolas.png"),
        ),
    ];
    let installed = tempfile::tempdir().unwrap();
    for (directory, probe) in maho_cli::config::SHIPPED_ASSETS {
        let source = &sources
            .iter()
            .find(|(name, _)| *name == directory)
            .expect("asset source")
            .1;
        std::fs::create_dir_all(installed.path().join(directory)).unwrap();
        std::fs::copy(source, installed.path().join(directory).join(probe))
            .expect("real shipped asset");
        let foreign = tempfile::tempdir().unwrap();
        let resolved =
            resolve_shipped_asset_dir(foreign.path(), installed.path(), directory, probe);
        assert!(
            resolved.join(probe).exists(),
            "{directory}: the running install answers"
        );
        assert_eq!(resolved, installed.path().join(directory));
        let relocated = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(relocated.path().join(directory)).unwrap();
        std::fs::write(relocated.path().join(directory).join(probe), "").unwrap();
        assert_eq!(
            resolve_shipped_asset_dir(relocated.path(), installed.path(), directory, probe),
            relocated.path().join(directory),
            "{directory}: the override wins"
        );
    }
}

#[test]
fn expand_tilde_path_normalizes_like_normalize_path() {
    let home = maho_core::config::home_dir();
    assert_eq!(maho_cli::config::expand_tilde_path("~"), home);
    assert_eq!(
        maho_cli::config::expand_tilde_path("/abs/out.html"),
        "/abs/out.html"
    );
}
