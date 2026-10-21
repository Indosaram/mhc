//! Port of senpi packages/agent/src/harness/utils/read-folders/.

pub mod brace_scanner;
pub mod compose;
pub mod header_protection;
pub mod lexical_context;
pub mod lexical_spans;
pub mod prepare;
pub mod tree_sitter;
pub mod types;

use std::sync::{Arc, OnceLock};
pub use types::*;
pub const READ_FOLDER_SELECTION_HEAD: &str = "6da8ef1361ef73928df8685e071f2a1826d3b17e";
pub const READ_FOLDER_SELECTION_SHA256: &str =
    "d7c502a1597e652f584877c0004ba6cde7c9393c8661ccbc388e3661ae66a634";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadFolderSelection {
    pub head: &'static str,
    pub selection_sha256: &'static str,
    pub wasm: bool,
    pub raw_reasons: &'static [(&'static str, &'static str)],
    pub languages: &'static [(&'static str, &'static str)],
}

/// Frozen measured selection, not a registry for enabling unmeasured grammars.
pub static READ_FOLDER_SELECTION: ReadFolderSelection = ReadFolderSelection {
    head: READ_FOLDER_SELECTION_HEAD,
    selection_sha256: READ_FOLDER_SELECTION_SHA256,
    wasm: true,
    raw_reasons: &[
        ("ts", "wasm_candidate_below_threshold"),
        ("tsx", "wasm_candidate_below_threshold"),
    ],
    languages: &[
        ("ts", "raw"),
        ("js", "wasm"),
        ("json", "heuristic"),
        ("tsx", "raw"),
        ("python", "unsupported"),
        ("rust", "unsupported"),
        ("go", "unsupported"),
        ("markdown", "prose_exempt"),
        ("txt", "prose_exempt"),
    ],
};

pub fn language_for_path(path: &str) -> Option<&'static str> {
    let name = path.rsplit(['/', '\\']).next()?.to_lowercase();
    let (_, extension) = name.rsplit_once('.')?;
    match extension {
        "ts" => Some("ts"),
        "js" => Some("js"),
        "json" => Some("json"),
        "tsx" => Some("tsx"),
        "py" => Some("python"),
        "rs" => Some("rust"),
        "go" => Some("go"),
        "md" | "markdown" | "mdown" | "mkd" | "mkdn" => Some("markdown"),
        "txt" => Some("txt"),
        _ => None,
    }
}
pub fn read_summary_engine_for_path(path: &str) -> Option<&'static str> {
    let language = language_for_path(path)?;
    READ_FOLDER_SELECTION
        .languages
        .iter()
        .find_map(|&(selected, engine)| (selected == language).then_some(engine))
}
pub fn is_read_summary_path(path: &str) -> bool {
    matches!(
        read_summary_engine_for_path(path),
        Some("heuristic" | "wasm")
    )
}
pub struct SelectedReadFolder;
pub static SELECTED_READ_FOLDER: SelectedReadFolder = SelectedReadFolder;
/// Shared handle to the selected folder. `prepare_read_folder` recognises the selection by
/// `Arc` identity (upstream compares against the frozen `selectedReadFolder` object), so every
/// default read must hand out this one allocation.
pub fn selected_read_folder() -> Arc<dyn ReadFolder> {
    static SELECTED: OnceLock<Arc<dyn ReadFolder>> = OnceLock::new();
    SELECTED
        .get_or_init(|| Arc::new(SelectedReadFolder))
        .clone()
}
impl ReadFolder for SelectedReadFolder {
    fn id(&self) -> &str {
        "measured-brace"
    }
    fn version(&self) -> &str {
        "3"
    }
    fn fold(&self, input: ReadFolderInput<'_>) -> ReadFolderResult {
        let language = language_for_path(input.path);
        if matches!(language, Some("markdown" | "txt")) {
            return ReadFolderResult::Unsupported {
                reason: "prose_exempt".into(),
            };
        }
        let Some(language @ ("ts" | "js" | "json")) = language else {
            return ReadFolderResult::Unsupported {
                reason: "unsupported_language".into(),
            };
        };
        if language == "json" && serde_json::from_str::<serde_json::Value>(input.text).is_err() {
            return ReadFolderResult::ParseFailure {
                reason: "invalid_json".into(),
            };
        }
        compose::compose_fold_result(
            input.text,
            brace_scanner::scan_braces(input.text, language, input.settings),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exported_metadata_matches_frozen_receipt() {
        let receipt: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/harness_read_receipt.json"
        )))
        .expect("frozen receipt must be valid JSON");
        let selection = &receipt["selection"]["default_read_selection"];

        let languages: serde_json::Map<String, serde_json::Value> = READ_FOLDER_SELECTION
            .languages
            .iter()
            .map(|&(language, engine)| (language.to_owned(), engine.into()))
            .collect();
        let raw_reasons: serde_json::Map<String, serde_json::Value> = READ_FOLDER_SELECTION
            .raw_reasons
            .iter()
            .map(|&(language, reason)| (language.to_owned(), reason.into()))
            .collect();

        assert_eq!(serde_json::Value::Object(languages), selection["languages"]);
        assert_eq!(
            serde_json::Value::Object(raw_reasons),
            selection["rawReasons"]
        );
        assert_eq!(
            serde_json::json!(READ_FOLDER_SELECTION.wasm),
            selection["wasm"]
        );
    }

    #[test]
    fn path_lookup_preserves_frozen_engine_choices() {
        let cases = [
            ("file.ts", Some("raw")),
            ("file.js", Some("wasm")),
            ("file.json", Some("heuristic")),
            ("file.tsx", Some("raw")),
            ("file.py", Some("unsupported")),
            ("file.rs", Some("unsupported")),
            ("file.go", Some("unsupported")),
            ("file.md", Some("prose_exempt")),
            ("file.txt", Some("prose_exempt")),
            ("file.unknown", None),
        ];

        for (path, expected) in cases {
            let engine = read_summary_engine_for_path(path);

            assert_eq!(engine, expected, "{path}");
        }
    }
}
