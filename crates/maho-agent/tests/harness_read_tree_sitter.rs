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

// --- read-tree-sitter-folds.test.ts: remaining cases (senpi 903b8336) ---

fn flatten(result: &ReadFolderResult) -> Vec<(usize, usize)> {
    let mut ranges: Vec<ReadFoldRange> = match result {
        ReadFolderResult::Parsed { ranges, .. } => ranges.clone(),
        _ => Vec::new(),
    };
    let mut index = 0;
    while index < ranges.len() {
        let children = ranges[index].children.clone();
        ranges.extend(children);
        index += 1;
    }
    let mut folds: Vec<(usize, usize)> = ranges
        .iter()
        .map(|range| (range.start_line, range.end_line))
        .collect();
    folds.sort();
    folds
}

fn declaration_corpus(declaration: impl Fn(usize) -> [String; 6]) -> String {
    (0..20)
        .map(|index| declaration(index).join("\n"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn typescript_source() -> String {
    declaration_corpus(|index| {
        [
            format!("export function example{index}(input: {{ value: number }}): number {{"),
            "  const doubled = input.value * 2;".into(),
            "  const label = `value ${doubled}`;".into(),
            "  const parsed = Number.parseInt(label.slice(6), 10);".into(),
            "  return parsed + doubled;".into(),
            "}".into(),
        ]
    })
}

fn tsx_source() -> String {
    declaration_corpus(|index| {
        [
            format!("export function Panel{index}({{ title }}: {{ title: string }}) {{"),
            r#"  const items = ["alpha", "bravo", "charlie"];"#.into(),
            "  const label = title.toUpperCase();".into(),
            "  const total = items.length;".into(),
            "  return <section title={label}>{total}</section>;".into(),
            "}".into(),
        ]
    })
}

fn javascript_source() -> String {
    declaration_corpus(|index| {
        [
            format!("function handler{index}(request) {{"),
            "  const headers = request.headers;".into(),
            r#"  const accepted = headers.accept || "*/*";"#.into(),
            "  const trimmed = accepted.trim();".into(),
            format!("  return {{ accepted: trimmed, index: {index} }};"),
            "}".into(),
        ]
    })
}

/// The compiler oracle (`typescriptOracle(...).allowed`, filtered by `minBodyLines`) for the
/// corpora above: each six-line declaration allows exactly its four-line body, and every
/// header/closing line is protected. Recorded here because the TS compiler oracle has no Rust
/// runtime; `harness_read_oracle.json` carries the general oracle corpus.
fn oracle_bodies() -> Vec<(usize, usize)> {
    (0..20)
        .map(|index| (6 * index + 2, 6 * index + 5))
        .collect()
}

fn fold_with(folder: &Arc<dyn ReadFolder>, extension: &str, text: &str) -> ReadFolderResult {
    folder.fold(ReadFolderInput {
        path: &format!("input.{extension}"),
        text,
        settings: READ_FOLD_SETTINGS,
    })
}

#[test]
fn produces_the_compiler_oracles_omittable_bodies() {
    for (language, extension, source) in [
        (TreeSitterLanguage::Ts, "ts", typescript_source()),
        (TreeSitterLanguage::Tsx, "tsx", tsx_source()),
        (TreeSitterLanguage::Js, "js", javascript_source()),
    ] {
        let engine = folder(language);
        assert_eq!(engine.id(), TREE_SITTER_FOLDER_ID);
        let parsed = fold_with(&engine, extension, &source);
        assert!(
            matches!(parsed, ReadFolderResult::Parsed { .. }),
            "{extension}"
        );
        let folds = flatten(&parsed);
        assert!(!folds.is_empty(), "{extension}");
        assert_eq!(folds, oracle_bodies(), "{extension}");
    }
}

#[test]
fn keeps_a_declaration_header_out_of_every_omitted_range() {
    let source = [
        "const registry = {",
        "  build(options) {",
        "    const mode = options.mode;",
        "    const retries = options.retries;",
        "    const timeout = options.timeout;",
        "    return { mode, retries, timeout };",
        "  },",
        "};",
    ]
    .join("\n");
    let engine = folder(TreeSitterLanguage::Js);
    assert_eq!(flatten(&fold_with(&engine, "js", &source)), vec![(3, 6)]);
}

#[test]
fn falls_back_to_the_heuristic_scan_on_a_malformed_file() {
    let source = format!("{}\nfunction broken( {{", javascript_source());
    let engine = folder(TreeSitterLanguage::Js);
    assert_eq!(
        fold_with(&engine, "js", &source),
        SELECTED_READ_FOLDER.fold(ReadFolderInput {
            path: "input.js",
            text: &source,
            settings: READ_FOLD_SETTINGS,
        })
    );
}

#[test]
fn falls_back_when_the_parse_budget_is_exhausted() {
    let source = javascript_source();
    let mut options = TreeSitterFolderOptions::new(Arc::new(SelectedReadFolder));
    options.cache = false;
    options.parse_budget_ms = 0;
    let exhausted = load_tree_sitter_folder(TreeSitterLanguage::Js, options).expect("grammar");
    assert_eq!(
        fold_with(&exhausted, "js", &source),
        SELECTED_READ_FOLDER.fold(ReadFolderInput {
            path: "input.js",
            text: &source,
            settings: READ_FOLD_SETTINGS,
        })
    );
    assert!(!flatten(&fold_with(&folder(TreeSitterLanguage::Js), "js", &source)).is_empty());
}

#[test]
fn prepares_the_grammar_backed_folder_for_exactly_the_languages_frozen_to_wasm() {
    let selected: Arc<dyn ReadFolder> = Arc::new(SelectedReadFolder);
    for path in [
        "input.ts",
        "input.tsx",
        "input.js",
        "input.json",
        "input.md",
        "input.py",
        "input.unknown",
    ] {
        let prepared =
            prepare_read_folder(path, Some(selected.clone()), &selected).expect("prepared folder");
        let expected = if read_summary_engine_for_path(path) == Some("wasm") {
            TREE_SITTER_FOLDER_ID.to_owned()
        } else {
            selected.id().to_owned()
        };
        assert_eq!(prepared.id(), expected, "{path}");
    }
    assert!(prepare_read_folder("input.js", None, &selected).is_none());
}
