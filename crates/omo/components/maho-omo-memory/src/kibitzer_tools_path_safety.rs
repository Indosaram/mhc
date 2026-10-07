//! Path safety for the sidecar tools (latest `kibitzer/tools/path-safety.ts`).

use std::path::{Component, Path, PathBuf};

use crate::kibitzer_tools_result::KibitzerRejectionCode;

pub enum PathCheck {
    Ok { path: String },
    Rejected { code: KibitzerRejectionCode, message: String },
}

/// Memory paths are repo-relative POSIX paths. The check is syntactic and fail-closed, and the
/// reserved root `system/` tree is invisible (nested `x/system/` is ordinary).
pub fn normalize_memory_path(input: &str) -> PathCheck {
    if input.trim().is_empty() {
        return PathCheck::Rejected { code: KibitzerRejectionCode::PathEmpty, message: "A memory path is required.".into() };
    }
    if input.contains('\\') {
        return PathCheck::Rejected { code: KibitzerRejectionCode::PathSeparator, message: "Memory paths use '/' separators.".into() };
    }
    if input.starts_with('/') {
        return PathCheck::Rejected { code: KibitzerRejectionCode::PathAbsolute, message: "Memory paths are relative to the memory repo.".into() };
    }
    let segments: Vec<&str> = input.split('/').filter(|segment| !segment.is_empty() && *segment != ".").collect();
    if segments.iter().any(|segment| *segment == "..") {
        return PathCheck::Rejected { code: KibitzerRejectionCode::PathTraversal, message: "Memory paths cannot contain '..' segments.".into() };
    }
    if segments.is_empty() {
        return PathCheck::Rejected { code: KibitzerRejectionCode::PathEmpty, message: "A memory path is required.".into() };
    }
    let path = segments.join("/");
    if path == "system" || path.starts_with("system/") {
        return PathCheck::Rejected { code: KibitzerRejectionCode::SystemPath, message: "The system/ tree is not readable by the sidecar.".into() };
    }
    PathCheck::Ok { path }
}

/// A workspace path must resolve inside the real workspace root. BOTH sides canonicalize, so a
/// symlink out of the root is rejected like `..` and absolute paths are. An unresolvable ROOT is a
/// rejection, never a silent lexical fallback: answering "inside" would fabricate containment.
pub fn resolve_workspace_path(root: &Path, input: &str) -> PathCheck {
    let real_root = match std::fs::canonicalize(root) {
        Ok(real_root) => real_root,
        Err(error) => {
            return PathCheck::Rejected { code: KibitzerRejectionCode::PathEscape, message: format!("the workspace root could not be resolved: {error}") };
        }
    };
    let lexical = lexical_resolve(&real_root, input);
    let target = std::fs::canonicalize(&lexical).unwrap_or(lexical);
    if target.starts_with(&real_root) {
        return PathCheck::Ok { path: target.to_string_lossy().into_owned() };
    }
    PathCheck::Rejected { code: KibitzerRejectionCode::PathEscape, message: format!("\"{input}\" resolves outside the workspace.") }
}

/// `path.resolve(root, input)`'s lexical half: `.` and `..` fold BEFORE any realpath, so a
/// non-existent `foo/../bar` resolves to `bar` instead of staying literal.
fn lexical_resolve(root: &Path, input: &str) -> PathBuf {
    let absolute = input.starts_with('/');
    let mut resolved: Vec<std::ffi::OsString> = if absolute {
        Vec::new()
    } else {
        root.components().map(|component| component.as_os_str().to_os_string()).collect()
    };
    for component in Path::new(input).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Normal(segment) => resolved.push(segment.to_os_string()),
            Component::RootDir | Component::Prefix(_) => resolved.clear(),
        }
    }
    let mut path = PathBuf::new();
    if absolute {
        path.push(std::path::MAIN_SEPARATOR.to_string());
    }
    for segment in resolved {
        path.push(segment);
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn given_a_dot_dot_segment_when_lexically_resolved_then_it_folds_before_realpath() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(lexical_resolve(root.path(), "foo/../bar"), root.path().join("bar"));
    }

    #[test]
    fn given_a_missing_root_when_resolved_then_it_is_a_rejection_not_a_fallback() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("missing");
        assert!(matches!(
            resolve_workspace_path(&missing, "a.md"),
            PathCheck::Rejected { code: KibitzerRejectionCode::PathEscape, .. }
        ));
    }

    #[test]
    fn given_an_absolute_path_inside_the_root_when_resolved_then_it_is_accepted() {
        let root = tempfile::tempdir().unwrap();
        let input = root.path().join("a.md").to_string_lossy().into_owned();
        assert!(matches!(resolve_workspace_path(root.path(), &input), PathCheck::Ok { .. }));
    }

    #[test]
    fn given_an_absolute_path_outside_the_root_when_resolved_then_it_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let input = outside.path().join("a.md").to_string_lossy().into_owned();
        assert!(matches!(
            resolve_workspace_path(root.path(), &input),
            PathCheck::Rejected { code: KibitzerRejectionCode::PathEscape, .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn given_a_symlink_escaping_the_root_when_resolved_then_it_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).expect("symlink");
        assert!(matches!(
            resolve_workspace_path(root.path(), "escape"),
            PathCheck::Rejected { code: KibitzerRejectionCode::PathEscape, .. }
        ));
    }

    #[test]
    fn given_a_traversal_memory_path_when_normalized_then_it_is_rejected() {
        assert!(matches!(
            normalize_memory_path("a/../b.md"),
            PathCheck::Rejected { code: KibitzerRejectionCode::PathTraversal, .. }
        ));
    }

    #[test]
    fn given_the_system_tree_when_normalized_then_it_is_rejected() {
        assert!(matches!(
            normalize_memory_path("system/x.md"),
            PathCheck::Rejected { code: KibitzerRejectionCode::SystemPath, .. }
        ));
    }

    #[test]
    fn given_a_plain_relative_path_when_normalized_then_it_is_accepted() {
        assert!(matches!(normalize_memory_path("a/b.md"), PathCheck::Ok { .. }));
    }
}
