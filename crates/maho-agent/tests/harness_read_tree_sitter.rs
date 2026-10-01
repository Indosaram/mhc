use maho_agent::harness::utils::read_folders::{
    prepare::prepare_read_folder,
    tree_sitter::{engine::*, syntax::*},
    *,
};
use std::sync::Arc;
fn folder(language: TreeSitterLanguage) -> Arc<dyn ReadFolder> {
    let mut options = TreeSitterFolderOptions::new(Arc::new(SelectedReadFolder));
    options.cache = false;
    load_tree_sitter_folder(language, options).expect("fixture invariant")
}
#[test]
fn native_grammar_folds_javascript_values() {
    let text = "const object = {\nalpha: 1,\nbravo: 2,\ncharlie: 3,\ndelta: 4,\necho: 5\n};";
    let result = folder(TreeSitterLanguage::Js).fold(ReadFolderInput {
        path: "x.js",
        text,
        settings: READ_FOLD_SETTINGS,
    });
    assert_eq!(
        result,
        ReadFolderResult::Parsed {
            text: text.into(),
            ranges: vec![ReadFoldRange {
                start_line: 2,
                end_line: 6,
                children: vec![]
            }]
        }
    );
}
#[test]
fn native_grammar_retains_typescript_headers() {
    let text = "function example(): {\nalpha: string;\nbravo: string;\ncharlie: string;\ndelta: string;\necho: string;\n} {\nreturn value;\n}";
    assert_eq!(
        folder(TreeSitterLanguage::Ts).fold(ReadFolderInput {
            path: "x.ts",
            text,
            settings: READ_FOLD_SETTINGS
        }),
        ReadFolderResult::Parsed {
            text: text.into(),
            ranges: vec![]
        }
    );
}
#[test]
fn absent_grammar_keeps_fallback() {
    let mut options = TreeSitterFolderOptions::new(Arc::new(SelectedReadFolder));
    options.cache = false;
    options.resolve_grammar = Some(Arc::new(|_| None));
    assert!(load_tree_sitter_folder(TreeSitterLanguage::Js, options).is_none());
}
#[test]
fn malformed_tree_uses_heuristic_fallback() {
    let text = "const object = [);";
    let input = ReadFolderInput {
        path: "x.js",
        text,
        settings: READ_FOLD_SETTINGS,
    };
    assert_eq!(
        folder(TreeSitterLanguage::Js).fold(input),
        SELECTED_READ_FOLDER.fold(input)
    );
}
#[test]
fn prepare_only_replaces_selected_eligible_folder() {
    let selected: Arc<dyn ReadFolder> = Arc::new(SelectedReadFolder);
    let custom: Arc<dyn ReadFolder> = Arc::new(SelectedReadFolder);
    assert!(Arc::ptr_eq(
        &prepare_read_folder("x.js", Some(custom.clone()), &selected).expect("fixture invariant"),
        &custom
    ));
    assert!(Arc::ptr_eq(
        &prepare_read_folder("x.ts", Some(selected.clone()), &selected).expect("fixture invariant"),
        &selected
    ));
    assert_eq!(
        prepare_read_folder("x.js", Some(selected.clone()), &selected)
            .expect("fixture invariant")
            .id(),
        TREE_SITTER_FOLDER_ID
    );
    assert!(prepare_read_folder("x.js", None, &selected).is_none());
}
#[test]
fn parser_reuse_does_not_retain_prior_source() {
    let folder = folder(TreeSitterLanguage::Js);
    let good = "const object = {\na:1,\nb:2,\nc:3,\nd:4,\ne:5\n};";
    let input = ReadFolderInput {
        path: "x.js",
        text: good,
        settings: READ_FOLD_SETTINGS,
    };
    let first = folder.fold(input);
    assert!(matches!(
        folder.fold(ReadFolderInput {
            text: "'unterminated",
            ..input
        }),
        ReadFolderResult::ParseFailure { .. }
    ));
    assert_eq!(folder.fold(input), first);
}
