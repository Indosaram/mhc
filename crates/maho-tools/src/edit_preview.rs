//! Port of `packages/coding-agent/src/core/tools/edit-preview.ts`.

use crate::definition::ToolError;
use crate::edit_diff::{
    Edit, EditDiffResult, apply_edits_to_normalized_content, generate_diff_string, normalize_to_lf,
};

/// Compute the diff for one or more edit operations without applying them.
/// Used for preview rendering in the TUI before the tool executes.
pub async fn compute_edits_diff(
    path: &str,
    edits: &[Edit],
    cwd: &std::path::Path,
) -> Result<EditDiffResult, ToolError> {
    let absolute = crate::path_utils::resolve_to_cwd(path, cwd);
    if let Err(error) = tokio::fs::File::open(&absolute).await {
        let code = match error.kind() {
            std::io::ErrorKind::NotFound => "ENOENT",
            std::io::ErrorKind::PermissionDenied => "EACCES",
            std::io::ErrorKind::NotADirectory => "ENOTDIR",
            _ => "UNKNOWN",
        };
        return Err(ToolError::Message(format!(
            "Could not edit file: {path}. Error code: {code}."
        )));
    }
    let bytes = tokio::fs::read(&absolute).await?;
    let text = String::from_utf8_lossy(&bytes);
    let normalized = normalize_to_lf(text.strip_prefix('\u{feff}').unwrap_or(&text));
    let result = apply_edits_to_normalized_content(&normalized, edits, path)?;
    Ok(generate_diff_string(
        &result.base_content,
        &result.new_content,
        4,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn compute_edits_diff_reports_a_missing_file_with_its_error_code() {
        let directory = tempfile::tempdir().expect("tempdir");
        let edits = [Edit {
            old_text: "a".into(),
            new_text: "b".into(),
        }];
        let error = compute_edits_diff("missing.txt", &edits, directory.path())
            .await
            .expect_err("missing");
        assert_eq!(
            error.to_string(),
            "Could not edit file: missing.txt. Error code: ENOENT."
        );
    }

    #[tokio::test]
    async fn compute_edits_diff_previews_without_writing() {
        let directory = tempfile::tempdir().expect("tempdir");
        std::fs::write(directory.path().join("f.txt"), "\u{feff}one\r\ntwo\r\n").expect("write");
        let edits = [Edit {
            old_text: "two".into(),
            new_text: "three".into(),
        }];
        let diff = compute_edits_diff("f.txt", &edits, directory.path())
            .await
            .expect("diff");
        assert_eq!(diff.first_changed_line, Some(2));
        assert!(diff.diff.contains("+2 three"));
        assert_eq!(
            std::fs::read_to_string(directory.path().join("f.txt")).expect("read"),
            "\u{feff}one\r\ntwo\r\n"
        );
    }
}
