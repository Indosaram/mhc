//! Post-edit LSP diagnostics: after `write`/`edit`/`apply_patch`, report errors in the touched files.
use crate::hooks::{HookFuture, Mutation, PostMutationHook};
use crate::tools::LspTools;
use serde_json::Value;
use std::collections::BTreeSet;

pub const MUTATION_TOOLS: [&str; 3] = ["write", "edit", "apply_patch"];
const CLEAN: &str = "No diagnostics found";
const UNSUPPORTED: &str = "No LSP server configured for extension:";

fn add(paths: &mut Vec<String>, seen: &mut BTreeSet<String>, value: Option<&Value>) {
    if let Some(path) = value
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        && seen.insert(path.to_owned())
    {
        paths.push(path.to_owned());
    }
}

/// Paths a mutation tool call touched, from its input (`path`, `filePath`, `paths`, `filePaths`,
/// patch headers in `input`, and `files`/`changes` entries).
pub fn extract_mutated_file_paths(input: &Value) -> Vec<String> {
    let mut paths = Vec::new();
    let mut seen = BTreeSet::new();
    add(&mut paths, &mut seen, input.get("path"));
    add(&mut paths, &mut seen, input.get("filePath"));
    for key in ["paths", "filePaths"] {
        for item in input
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            add(&mut paths, &mut seen, Some(item));
        }
    }
    if let Some(patch) = input.get("input").and_then(Value::as_str) {
        for line in patch.split('\n') {
            let header = ["*** Add File: ", "*** Update File: ", "*** Move to: "]
                .iter()
                .find_map(|prefix| line.strip_prefix(prefix));
            if let Some(path) = header {
                let value = Value::String(path.trim().to_owned());
                add(&mut paths, &mut seen, Some(&value));
            }
        }
    }
    for key in ["files", "changes"] {
        for item in input
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            for field in ["path", "filePath", "movePath"] {
                add(&mut paths, &mut seen, item.get(field));
            }
        }
    }
    paths
}

/// Whether a diagnostics report needs no attention.
pub fn is_clean(report: &str) -> bool {
    report.is_empty() || report == CLEAN || report.starts_with(UNSUPPORTED)
}

/// Appended block for one file with errors.
pub fn error_block(file: &str, report: &str) -> String {
    format!("\n\nLSP errors detected in {file}, please fix:\n{report}")
}

/// [`PostMutationHook`] running `lsp_diagnostics` (errors only) on each mutated file.
pub struct LspDiagnosticsHook {
    pub tools: LspTools,
}

impl PostMutationHook for LspDiagnosticsHook {
    fn id(&self) -> &str {
        "lsp-diagnostics"
    }

    fn after_mutation<'a>(&'a self, mutation: &'a Mutation) -> HookFuture<'a, Option<String>> {
        Box::pin(async move {
            if !MUTATION_TOOLS.contains(&mutation.tool_name.as_str()) {
                return None;
            }
            let mut blocks = String::new();
            for file in &mutation.files {
                let report = match self
                    .tools
                    .diagnostics(&mutation.cwd, file, Some("error"))
                    .await
                {
                    Ok(output) => output.text,
                    Err(_) => continue,
                };
                let report = report.trim();
                if !is_clean(report) {
                    blocks.push_str(&error_block(file, report));
                }
            }
            (!blocks.is_empty()).then_some(blocks)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mutated_paths_from_all_input_shapes() {
        let input = json!({
            "path": "a.ts", "filePath": "a.ts", "paths": ["b.ts", 3], "filePaths": ["c.ts"],
            "input": "*** Begin Patch\n*** Add File: d.ts\n*** Update File: e.ts\n*** Move to: f.ts\n*** Delete File: g.ts\n",
            "files": [{"path": "h.ts", "movePath": "i.ts"}], "changes": [{"filePath": "j.ts"}]
        });
        assert_eq!(
            extract_mutated_file_paths(&input),
            [
                "a.ts", "b.ts", "c.ts", "d.ts", "e.ts", "f.ts", "h.ts", "i.ts", "j.ts"
            ]
        );
    }

    #[test]
    fn clean_reports() {
        assert!(is_clean(""));
        assert!(is_clean("No diagnostics found"));
        assert!(is_clean("No LSP server configured for extension: .md\n..."));
        assert!(!is_clean("error at 1:0: bad"));
        assert_eq!(
            error_block("a.ts", "x"),
            "\n\nLSP errors detected in a.ts, please fix:\nx"
        );
    }
}
