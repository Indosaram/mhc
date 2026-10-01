use maho_agent::harness::utils::read_folders::{
    tree_sitter::{engine::*, syntax::TreeSitterLanguage},
    *,
};
fn receipt() -> serde_json::Value {
    serde_json::from_str(include_str!("fixtures/harness_read_receipt.json"))
        .expect("fixture invariant")
}
#[test]
fn binds_registry_to_reproducible_receipt() {
    let receipt = receipt();
    let selection = &receipt["selection"];
    assert_eq!(receipt["receiptSha256"], READ_FOLDER_SELECTION_SHA256);
    assert_eq!(selection["head_sha"], READ_FOLDER_SELECTION_HEAD);
    assert_eq!(
        selection["default_read_selection"]["folder"]["id"],
        SELECTED_READ_FOLDER.id()
    );
    assert_eq!(
        selection["default_read_selection"]["folder"]["version"],
        SELECTED_READ_FOLDER.version()
    );
    assert_eq!(
        selection["enumeration"]["grammarSha256"],
        receipt["grammarSha256"]
    );
    assert_eq!(
        selection["enumeration"]["counterexamples"],
        serde_json::json!([])
    );
    assert_eq!(
        selection["candidate_sources_sha256"],
        receipt["sourceHashes"]
    );
    for language in ["ts", "js", "json"] {
        let row = selection["languages"]
            .as_array()
            .expect("fixture invariant")
            .iter()
            .find(|r| r["language"] == language)
            .expect("fixture invariant");
        assert_eq!(row["invalid_boundaries"], serde_json::json!([]));
        assert_eq!(
            row["engine"],
            read_summary_engine_for_path(&format!("input.{language}")).expect("fixture invariant")
        );
    }
}
#[test]
fn freezes_per_language_engines_from_measurement() {
    let receipt = receipt();
    let selection = &receipt["selection"];
    for (language, extension) in [
        ("ts", "ts"),
        ("tsx", "tsx"),
        ("js", "js"),
        ("json", "json"),
        ("python", "py"),
        ("rust", "rs"),
        ("go", "go"),
        ("markdown", "md"),
        ("txt", "txt"),
    ] {
        let engine =
            read_summary_engine_for_path(&format!("x.{extension}")).expect("fixture invariant");
        assert_eq!(
            selection["default_read_selection"]["languages"][language],
            engine
        );
        if engine == "raw" {
            let reason = selection["default_read_selection"]["rawReasons"][language]
                .as_str()
                .expect("fixture invariant");
            assert!(!reason.contains("pending_owner"));
            let row = selection["languages"]
                .as_array()
                .expect("fixture invariant")
                .iter()
                .find(|r| r["language"] == language)
                .expect("fixture invariant");
            assert_eq!(row["reason"], reason);
        }
    }
    assert_eq!(selection["default_read_selection"]["wasm"], true);
}
#[test]
fn ships_native_grammar_for_selected_language() {
    let mut options = TreeSitterFolderOptions::new(std::sync::Arc::new(SelectedReadFolder));
    options.cache = false;
    let folder =
        load_tree_sitter_folder(TreeSitterLanguage::Js, options).expect("fixture invariant");
    assert_eq!(folder.id(), TREE_SITTER_FOLDER_ID);
    let text = "function f() {\na();\nb();\nc();\nd();\n}";
    assert!(
        matches!(folder.fold(ReadFolderInput { path:"x.js",text,settings:READ_FOLD_SETTINGS }),ReadFolderResult::Parsed { ranges,.. } if !ranges.is_empty())
    );
}
