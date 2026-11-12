//! Confined resolution of memory paths: everything a memory write may touch stays under the root.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-paths.md`. Two modes exist because two callers do:
//! the memory tool addresses a Markdown block by label (`system/contacts`), while repository mode
//! addresses anything git tracks, directories included.
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryPathError {
    pub message: String,
}

impl std::fmt::Display for MemoryPathError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for MemoryPathError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidateMemoryPathOptions<'a> {
    pub tool_path: bool,
    pub field_name: &'a str,
}

impl Default for ValidateMemoryPathOptions<'_> {
    fn default() -> Self {
        Self {
            tool_path: true,
            field_name: "path",
        }
    }
}

/// Whether `path` names a memory block the prompt index may project.
pub fn is_memory_content_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    if normalized.contains(['\n', '\r', '\u{2028}', '\u{2029}']) {
        return false;
    }
    let body = normalized.strip_prefix("memory/").unwrap_or(&normalized);
    if let Some(rest) = body.strip_prefix("skills/") {
        return rest.len() > "/SKILL.md".len() && rest.ends_with("/SKILL.md");
    }
    ["system/", "reference/", "people/"].iter().any(|prefix| {
        body.strip_prefix(prefix)
            .is_some_and(|rest| rest.ends_with(".md"))
    })
}

pub fn validate_memory_path(
    memory_root: &Path,
    input_path: &str,
    options: ValidateMemoryPathOptions<'_>,
) -> Result<PathBuf, MemoryPathError> {
    let root = memory_root.to_path_buf();
    let field_name = options.field_name;
    let tool_path = options.tool_path;
    let raw = input_path.trim();

    if raw.is_empty() {
        return Err(invalid(&format!(
            "'{field_name}' must be a non-empty string"
        )));
    }
    if raw.contains('\0') {
        return Err(invalid(&format!(
            "'{field_name}' contains invalid null bytes"
        )));
    }
    if raw.starts_with("~/") || raw.starts_with("$HOME/") {
        return Err(invalid(&format!(
            "'{field_name}' must be a memory-relative path, not a home-relative filesystem path"
        )));
    }

    let mut relative_input = raw.to_string();
    if Path::new(raw).is_absolute() || is_windows_absolute(raw) {
        let abs_raw = Path::new(raw);
        let rel_to_memory = if let Ok(stripped) = abs_raw.strip_prefix(&root) {
            stripped.to_path_buf()
        } else {
            let canon_root = root.canonicalize().unwrap_or_else(|_| root.clone());
            let canon_raw = abs_raw
                .canonicalize()
                .unwrap_or_else(|_| abs_raw.to_path_buf());
            match canon_raw.strip_prefix(&canon_root) {
                Ok(stripped) => stripped.to_path_buf(),
                Err(_) => return Err(invalid(&prefix_error(&root))),
            }
        };

        if rel_to_memory.as_os_str().is_empty() || !is_confined_relative(&rel_to_memory) {
            return Err(invalid(&prefix_error(&root)));
        }
        relative_input = rel_to_memory.to_string_lossy().to_string();
    }

    let normalized = normalize_relative_path(&relative_input, field_name, tool_path)?;
    let target = root.join(&normalized);
    let Ok(rel_to_root) = target.strip_prefix(&root) else {
        return Err(invalid("resolved path escapes memory directory"));
    };

    if rel_to_root.as_os_str().is_empty() || !is_confined_relative(rel_to_root) {
        return Err(invalid("resolved path escapes memory directory"));
    }

    assert_real_ancestor_confined(&root, &target)?;
    Ok(target)
}

pub fn validate_repository_path(
    memory_root: &Path,
    input_path: &str,
) -> Result<PathBuf, MemoryPathError> {
    validate_repository_path_with_field(memory_root, input_path, "path")
}

pub fn validate_repository_path_with_field(
    memory_root: &Path,
    input_path: &str,
    field_name: &str,
) -> Result<PathBuf, MemoryPathError> {
    validate_memory_path(
        memory_root,
        input_path,
        ValidateMemoryPathOptions {
            tool_path: false,
            field_name,
        },
    )
}

fn normalize_relative_path(
    input_path: &str,
    field_name: &str,
    tool_path: bool,
) -> Result<String, MemoryPathError> {
    let normalized = input_path.trim().replace('\\', "/");
    if normalized.starts_with('/') {
        return Err(invalid(&format!(
            "'{field_name}' must be a relative path like system/contacts.md"
        )));
    }

    let path_no_mem = normalized.strip_prefix("memory/").unwrap_or(&normalized);
    if path_no_mem.is_empty() {
        return Err(invalid(&format!(
            "'{field_name}' resolves to an empty memory label"
        )));
    }

    let mut path = path_no_mem.to_string();
    if tool_path {
        let ext = extension_of(&path);
        if !ext.is_empty() && ext != ".md" {
            return Err(invalid(&format!(
                "'{field_name}' must target a lowercase .md markdown file"
            )));
        }
        if path.ends_with(".md") {
            path.truncate(path.len() - 3);
        }
    }

    let segments: Vec<&str> = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    if segments.is_empty() {
        return Err(invalid(&format!(
            "'{field_name}' resolves to an empty memory label"
        )));
    }
    for segment in &segments {
        if *segment == "." || *segment == ".." {
            return Err(invalid(&format!(
                "'{field_name}' contains invalid path traversal segment"
            )));
        }
        if segment.eq_ignore_ascii_case(".git") {
            return Err(invalid(&format!(
                "'{field_name}' cannot target .git administration paths"
            )));
        }
    }

    let repo_path = segments.join("/");
    if tool_path {
        Ok(format!("{repo_path}.md"))
    } else {
        Ok(repo_path)
    }
}

/// Walk up to the first path that exists and require its real location to stay under the root.
///
/// The whole point is the component that exists: a missing leaf cannot escape on its own, but a
/// symlinked ancestor redirects the write elsewhere, and `..` inside a symlink target is invisible to
/// a purely lexical check.
fn assert_real_ancestor_confined(root: &Path, target: &Path) -> Result<(), MemoryPathError> {
    let root_real = root
        .canonicalize()
        .map_err(|err| invalid(&format!("memory root cannot be resolved: {err}")))?;

    let mut candidate = target.to_path_buf();
    loop {
        match std::fs::symlink_metadata(&candidate) {
            Ok(_) => {
                let candidate_real = candidate.canonicalize().map_err(|err| {
                    invalid(&format!("path contains an unresolved symlink: {err}"))
                })?;
                if !candidate_real.starts_with(&root_real) {
                    return Err(invalid("path escapes memory root through a symlink"));
                }
                return Ok(());
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let Some(parent) = candidate.parent().map(Path::to_path_buf) else {
                    return Err(invalid("path has no existing ancestor inside memory root"));
                };
                if parent == candidate {
                    return Err(invalid("path has no existing ancestor inside memory root"));
                }
                candidate = parent;
            }
            Err(err) => {
                return Err(invalid(&format!(
                    "path ancestor cannot be inspected: {err}"
                )));
            }
        }
    }
}

fn is_confined_relative(path: &Path) -> bool {
    !path.to_string_lossy().starts_with("..") && !path.is_absolute()
}

fn prefix_error(root: &Path) -> String {
    format!(
        "The memory tool can only be used to modify files in {{{}}} or provided as a relative path",
        root.display()
    )
}

fn invalid(message: &str) -> MemoryPathError {
    MemoryPathError {
        message: format!("memory path: {message}"),
    }
}

fn is_windows_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

fn extension_of(path: &str) -> String {
    let filename = match path.rfind('/') {
        Some(pos) => &path[pos + 1..],
        None => path,
    };
    match filename.rfind('.') {
        Some(dot) => filename[dot..].to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn root() -> TempDir {
        let dir = TempDir::new().expect("tempdir");
        fs::create_dir_all(dir.path().join("system")).expect("system dir");
        dir
    }

    fn tool_path(root: &Path, input: &str) -> Result<PathBuf, MemoryPathError> {
        validate_memory_path(root, input, ValidateMemoryPathOptions::default())
    }

    #[test]
    fn a_memory_label_resolves_to_a_markdown_file() {
        let dir = root();
        let resolved = tool_path(dir.path(), "system/contacts").expect("resolves");
        assert_eq!(resolved, dir.path().join("system/contacts.md"));
    }

    #[test]
    fn a_markdown_suffix_is_accepted_and_normalized_away() {
        let dir = root();
        assert_eq!(
            tool_path(dir.path(), "system/contacts.md").expect("resolves"),
            dir.path().join("system/contacts.md")
        );
    }

    #[test]
    fn a_memory_prefix_is_stripped() {
        let dir = root();
        assert_eq!(
            tool_path(dir.path(), "memory/system/contacts").expect("resolves"),
            dir.path().join("system/contacts.md")
        );
    }

    #[test]
    fn a_repeated_separator_is_collapsed() {
        let dir = root();
        assert_eq!(
            tool_path(dir.path(), "system//nested///note").expect("resolves"),
            dir.path().join("system/nested/note.md")
        );
    }

    #[test]
    fn an_absolute_path_inside_the_root_resolves() {
        let dir = root();
        let absolute = dir.path().join("system/contacts.md");
        assert_eq!(
            tool_path(dir.path(), &absolute.to_string_lossy()).expect("resolves"),
            absolute
        );
    }

    #[test]
    fn an_absolute_path_outside_the_root_is_refused() {
        let dir = root();
        // A platform-free literal is not outside the root everywhere: `/etc/passwd` is a rooted path
        // on the current drive on Windows, and `C:\...` is a relative name on Unix. Build a sibling
        // of the root instead, which is genuinely outside on every platform.
        let outside = dir
            .path()
            .parent()
            .expect("root has a parent")
            .join("outside-the-root.md");
        let err = tool_path(dir.path(), &outside.to_string_lossy()).expect_err("must refuse");
        assert!(
            err.message.contains("can only be used to modify files"),
            "{}",
            err.message
        );
    }

    #[test]
    fn a_traversal_segment_is_refused() {
        let dir = root();
        let err = tool_path(dir.path(), "system/../../escape").expect_err("must refuse");
        assert!(err.message.contains("path traversal"), "{}", err.message);
    }

    #[test]
    fn a_single_dot_segment_is_refused() {
        let dir = root();
        let err = tool_path(dir.path(), "system/./note").expect_err("must refuse");
        assert!(err.message.contains("path traversal"), "{}", err.message);
    }

    #[test]
    fn a_git_path_is_refused_in_any_case() {
        let dir = root();
        for input in ["system/.git/config", "system/.GIT/config"] {
            let err = tool_path(dir.path(), input).expect_err("must refuse");
            assert!(err.message.contains(".git"), "{} {}", input, err.message);
        }
    }

    #[test]
    fn a_home_relative_path_is_refused() {
        let dir = root();
        for input in ["~/secrets.md", "$HOME/secrets.md"] {
            let err = tool_path(dir.path(), input).expect_err("must refuse");
            assert!(err.message.contains("memory-relative"), "{}", err.message);
        }
    }

    #[test]
    fn an_empty_or_null_path_is_refused() {
        let dir = root();
        assert!(tool_path(dir.path(), "   ").is_err());
        assert!(tool_path(dir.path(), "system/a\0b").is_err());
    }

    #[test]
    fn a_non_markdown_extension_is_refused_for_a_tool_path() {
        let dir = root();
        let err = tool_path(dir.path(), "system/notes.txt").expect_err("must refuse");
        assert!(err.message.contains(".md"), "{}", err.message);
    }

    #[test]
    fn an_uppercase_markdown_extension_is_refused_for_a_tool_path() {
        let dir = root();
        let err = tool_path(dir.path(), "system/notes.MD").expect_err("must refuse");
        assert!(err.message.contains(".md"), "{}", err.message);
    }

    #[test]
    fn a_repository_path_accepts_any_extension_and_directory() {
        let dir = root();
        let resolved =
            validate_repository_path(dir.path(), "assets/diagram.png").expect("resolves");
        assert_eq!(resolved, dir.path().join("assets/diagram.png"));
        assert!(validate_repository_path(dir.path(), "system").is_ok());
    }

    #[test]
    fn a_repository_path_still_refuses_traversal() {
        let dir = root();
        assert!(validate_repository_path(dir.path(), "../outside").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_directory_that_escapes_the_root_is_refused() {
        let dir = root();
        let outside = TempDir::new().expect("outside");
        std::os::unix::fs::symlink(outside.path(), dir.path().join("system/escape"))
            .expect("symlink");

        let err = tool_path(dir.path(), "system/escape/note").expect_err("must refuse");
        assert!(
            err.message.contains("symlink"),
            "expected a symlink refusal, got {}",
            err.message
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_that_stays_inside_the_root_is_allowed() {
        let dir = root();
        std::os::unix::fs::symlink(dir.path().join("system"), dir.path().join("system/alias"))
            .expect("symlink");
        assert!(tool_path(dir.path(), "system/alias/note").is_ok());
    }

    #[test]
    fn a_missing_leaf_under_a_confined_ancestor_resolves() {
        let dir = root();
        let resolved = tool_path(dir.path(), "system/deep/missing/note").expect("resolves");
        assert!(resolved.ends_with("system/deep/missing/note.md"));
    }

    #[test]
    fn a_windows_absolute_path_outside_the_root_is_refused() {
        let dir = root();
        let err = tool_path(dir.path(), "C:\\Windows\\System32\\config").expect_err("must refuse");
        assert!(
            err.message.contains("can only be used to modify files"),
            "{}",
            err.message
        );
    }

    #[test]
    fn a_content_path_requires_a_known_prefix_and_markdown() {
        assert!(is_memory_content_path("system/contacts.md"));
        assert!(is_memory_content_path("memory/system/contacts.md"));
        assert!(is_memory_content_path("reference/notes.md"));
        assert!(is_memory_content_path("people/human/card.md"));
        assert!(!is_memory_content_path("system/contacts.txt"));
        assert!(!is_memory_content_path("elsewhere/contacts.md"));
        assert!(!is_memory_content_path("system/contacts.md\nsystem/x.md"));
    }

    #[test]
    fn a_skill_path_must_have_a_name_before_the_marker() {
        assert!(is_memory_content_path("skills/writer/SKILL.md"));
        assert!(!is_memory_content_path("skills/SKILL.md"));
        assert!(!is_memory_content_path("skills/writer/README.md"));
    }

    #[test]
    fn a_backslash_path_is_read_as_separators() {
        let dir = root();
        assert_eq!(
            tool_path(dir.path(), "system\\note").expect("resolves"),
            dir.path().join("system/note.md")
        );
    }
}
