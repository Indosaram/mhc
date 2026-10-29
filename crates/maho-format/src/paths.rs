//! Spec §2: paths named by a tool call, resolved against the cwd and normalized lexically.
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

const PATCH_HEADERS: [&str; 3] = ["*** Add File: ", "*** Update File: ", "*** Move to: "];

#[derive(Default)]
struct Collected(Vec<String>);

impl Collected {
    fn push(&mut self, value: &str) {
        if !value.is_empty() && !self.0.iter().any(|seen| seen == value) {
            self.0.push(value.to_owned());
        }
    }
    fn push_value(&mut self, value: Option<&Value>) {
        if let Some(text) = value.and_then(Value::as_str) {
            self.push(text);
        }
    }
    fn push_array(&mut self, value: Option<&Value>) {
        for item in value.and_then(Value::as_array).into_iter().flatten() {
            self.push_value(Some(item));
        }
    }
    fn push_objects(&mut self, value: Option<&Value>, fields: &[&str]) {
        for item in value.and_then(Value::as_array).into_iter().flatten() {
            for field in fields {
                self.push_value(item.get(*field));
            }
        }
    }
}

/// Paths as the tool reported them, in spec order, without empty strings or exact duplicates.
/// `details` is the tool result's details (only read for `apply_patch`).
pub fn collect_paths(tool_name: &str, input: &Value, details: Option<&Value>) -> Vec<String> {
    let mut out = Collected::default();
    out.push_value(input.get("path"));
    out.push_value(input.get("filePath"));
    out.push_array(input.get("paths"));
    out.push_array(input.get("filePaths"));
    if let Some(patch) = input.get("input").and_then(Value::as_str) {
        for line in patch.lines() {
            if let Some(rest) = PATCH_HEADERS
                .iter()
                .find_map(|header| line.strip_prefix(header))
            {
                out.push(rest.trim());
            }
        }
    }
    if tool_name == "apply_patch"
        && let Some(details) = details
    {
        let result = details.get("result");
        out.push_array(result.and_then(|result| result.get("appliedFiles")));
        let operations = result
            .and_then(|result| result.get("details"))
            .and_then(|details| details.get("appliedOperations"))
            .and_then(Value::as_array);
        for operation in operations.into_iter().flatten() {
            let preview = operation.get("preview");
            out.push_value(preview.and_then(|preview| preview.get("filePath")));
            out.push_value(preview.and_then(|preview| preview.get("movePath")));
        }
    }
    out.push_objects(input.get("files"), &["path", "filePath", "movePath"]);
    out.push_objects(input.get("changes"), &["path", "filePath", "movePath"]);
    out.0
}

/// `path` against `cwd` (absolute paths stay), with `.` dropped and `..` popping a component.
pub fn resolve(cwd: &Path, path: &str) -> PathBuf {
    let raw = Path::new(path);
    let joined = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        cwd.join(raw)
    };
    normalize(&joined)
}

/// Lexical normalization; no symlink resolution.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                let at_root = matches!(
                    out.components().next_back(),
                    None | Some(Component::RootDir | Component::Prefix(_))
                );
                if !at_root {
                    out.pop();
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `path` relative to `cwd` with `/` separators (the absolute path when outside `cwd`).
pub fn display_relative(cwd: &Path, path: &Path) -> String {
    match path.strip_prefix(cwd) {
        Ok(relative) => relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
        Err(_) => path.to_string_lossy().replace('\\', "/"),
    }
}
