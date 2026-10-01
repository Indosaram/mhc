//! Port of senpi packages/agent/src/harness/utils/read-folders/.

pub mod brace_scanner;
pub mod compose;
pub mod header_protection;
pub mod lexical_context;
pub mod lexical_spans;
pub mod prepare;
pub mod tree_sitter;
pub mod types;

pub use types::*;
pub const READ_FOLDER_SELECTION_HEAD: &str = "ec63eb5ee999da83b9e5d3e37b83d1998707f44c";
pub const READ_FOLDER_SELECTION_SHA256: &str =
    "07388b6a393cc05c78533fa701d536d5e617a38e2293b10c0a281a55395cc8a3";
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
    match language_for_path(path)? {
        "ts" | "tsx" => Some("raw"),
        "js" => Some("wasm"),
        "json" => Some("heuristic"),
        "markdown" | "txt" => Some("prose_exempt"),
        _ => Some("unsupported"),
    }
}
pub fn is_read_summary_path(path: &str) -> bool {
    matches!(
        read_summary_engine_for_path(path),
        Some("heuristic" | "wasm")
    )
}
pub struct SelectedReadFolder;
pub static SELECTED_READ_FOLDER: SelectedReadFolder = SelectedReadFolder;
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
