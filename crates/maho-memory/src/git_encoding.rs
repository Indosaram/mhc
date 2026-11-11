//! Why a dirty memory file cannot be read as text.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-git-encoding.md`.

use std::path::Path;

/// Describe each markdown file in `porcelain` whose bytes are not text a reader can trust.
///
/// Only markdown is inspected: binary memory artifacts are written as bytes on purpose, so a NUL in one of
/// them is the format, not a defect.
pub fn describe_dirty_markdown_encoding_issues(root: &Path, porcelain: &str) -> Vec<String> {
    crate::git_status::parse(porcelain)
        .map(|entries| {
            entries
                .into_iter()
                .filter(|entry| entry.is_readable_content())
                .map(|entry| entry.path)
                .filter(|path| path.ends_with(".md"))
                .filter_map(|path| describe_markdown_encoding_issue(root, &path))
                .collect()
        })
        .unwrap_or_default()
}

/// Name the encoding of a markdown file when it is not UTF-8.
///
/// The order matters: a UTF-16 BOM also contains a NUL byte in its second half, so the BOM is checked
/// first or every UTF-16 file would be reported as unknown NULs. A file that cannot be read is not
/// reported at all — a missing file is a status problem, not an encoding one, and inventing a
/// diagnosis for it would hide the real error.
fn describe_markdown_encoding_issue(root: &Path, relative_path: &str) -> Option<String> {
    let bytes = std::fs::read(root.join(relative_path)).ok()?;
    if bytes.len() >= 2 && bytes[0] == 0xff && bytes[1] == 0xfe {
        return Some(format!("{relative_path} has UTF-16 LE BOM"));
    }
    if bytes.len() >= 2 && bytes[0] == 0xfe && bytes[1] == 0xff {
        return Some(format!("{relative_path} has UTF-16 BE BOM"));
    }
    if bytes.contains(&0) {
        return Some(format!(
            "{relative_path} contains NUL bytes, possibly UTF-16"
        ));
    }
    None
}

/// A refusal to commit because the repository holds changes the caller did not make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnrelatedChanges {
    pub porcelain: String,
    pub encoding_diagnostics: Vec<String>,
}

impl std::fmt::Display for UnrelatedChanges {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "repository has changes outside the intended paths:\n{}",
            self.porcelain.trim_end()
        )?;
        for diagnostic in &self.encoding_diagnostics {
            write!(formatter, "\n  {diagnostic}")?;
        }
        Ok(())
    }
}

impl std::error::Error for UnrelatedChanges {}

/// Fail when the status contains a change to any path outside `expected_paths`.
///
/// An empty status passes. A path that is expected is allowed whether it is added, modified or deleted.
pub fn assert_no_unrelated_changes<F>(
    root: &Path,
    expected_paths: &[String],
    get_status: F,
) -> Result<(), UnrelatedChanges>
where
    F: FnOnce() -> Result<String, UnrelatedChanges>,
{
    let porcelain = get_status()?;
    if porcelain.trim().is_empty() {
        return Ok(());
    }
    let allowed: std::collections::BTreeSet<&str> =
        expected_paths.iter().map(String::as_str).collect();
    let unrelated = match crate::git_status::parse(&porcelain) {
        Ok(entries) => entries
            .iter()
            .any(|entry| !allowed.contains(entry.path.as_str())),
        Err(_) => true,
    };
    if !unrelated {
        return Ok(());
    }
    Err(UnrelatedChanges {
        encoding_diagnostics: describe_dirty_markdown_encoding_issues(root, &porcelain),
        porcelain,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tempdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "maho-gitenc-{name}-{}",
            crate::random_ids::random_id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_utf16le_markdown_file_is_named() {
        let dir = tempdir("le");
        std::fs::write(dir.join("a.md"), [0xff, 0xfe, b'h', 0x00]).unwrap();
        let found = describe_dirty_markdown_encoding_issues(&dir, " M a.md\n");
        assert_eq!(found, vec!["a.md has UTF-16 LE BOM".to_string()]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_utf16be_markdown_file_is_named() {
        let dir = tempdir("be");
        std::fs::write(dir.join("b.md"), [0xfe, 0xff, 0x00, b'h']).unwrap();
        let found = describe_dirty_markdown_encoding_issues(&dir, " M b.md\n");
        assert_eq!(found, vec!["b.md has UTF-16 BE BOM".to_string()]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_nul_byte_without_a_bom_is_reported_as_possible_utf16() {
        let dir = tempdir("nul");
        std::fs::write(dir.join("c.md"), [b'h', 0x00, b'i']).unwrap();
        let found = describe_dirty_markdown_encoding_issues(&dir, " M c.md\n");
        assert_eq!(
            found,
            vec!["c.md contains NUL bytes, possibly UTF-16".to_string()]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_clean_utf8_markdown_file_is_not_reported() {
        let dir = tempdir("clean");
        std::fs::write(dir.join("d.md"), "한글 정상".as_bytes()).unwrap();
        assert!(describe_dirty_markdown_encoding_issues(&dir, " M d.md\n").is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn non_markdown_files_are_not_inspected() {
        let dir = tempdir("binary");
        std::fs::write(dir.join("blob.bin"), [0x00, 0x01]).unwrap();
        assert!(describe_dirty_markdown_encoding_issues(&dir, " M blob.bin\n").is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_deleted_markdown_file_is_not_read() {
        let dir = tempdir("deleted");
        let found = describe_dirty_markdown_encoding_issues(&dir, " D gone.md\n");
        assert!(found.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_renamed_file_is_inspected_at_its_new_path() {
        let dir = tempdir("renamed");
        std::fs::write(dir.join("new.md"), [0xff, 0xfe]).unwrap();
        let found = describe_dirty_markdown_encoding_issues(&dir, "R  old.md -> new.md\n");
        assert_eq!(found, vec!["new.md has UTF-16 LE BOM".to_string()]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_empty_status_passes() {
        let dir = tempdir("empty");
        let result = assert_no_unrelated_changes(&dir, &[], || Ok(String::new()));
        assert!(result.is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_change_to_an_expected_path_passes() {
        let dir = tempdir("expected");
        std::fs::write(dir.join("note.md"), "ok").unwrap();
        let result = assert_no_unrelated_changes(&dir, &["note.md".to_string()], || {
            Ok(" M note.md\n".to_string())
        });
        assert!(result.is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_change_to_another_path_is_refused_with_the_porcelain() {
        let dir = tempdir("unrelated");
        std::fs::write(dir.join("stray.md"), [0xff, 0xfe]).unwrap();
        let result =
            assert_no_unrelated_changes(&dir, &[], || Ok(" M note.md\n M stray.md\n".to_string()));
        let error = result.unwrap_err();
        assert!(error.porcelain.contains("note.md"));
        assert_eq!(
            error.encoding_diagnostics,
            vec!["stray.md has UTF-16 LE BOM".to_string()]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_unparseable_status_is_refused_rather_than_assumed_clean() {
        let dir = tempdir("garbage");
        let result = assert_no_unrelated_changes(&dir, &[], || Ok("X\n".to_string()));
        assert!(result.is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_display_names_the_refusal_and_the_diagnostics() {
        let error = UnrelatedChanges {
            porcelain: " M other.md\n".to_string(),
            encoding_diagnostics: vec!["other.md has UTF-16 LE BOM".to_string()],
        };
        let text = error.to_string();
        assert!(text.contains("outside the intended paths"));
        assert!(text.contains("other.md has UTF-16 LE BOM"));
    }
}
