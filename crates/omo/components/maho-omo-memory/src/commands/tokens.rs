//! bytes/4 system-prompt token estimate over the working-tree system/ directory.
//! Port of `components/memory/commands/tokens.ts` at pin 77f3067f1.

use std::path::{Path, PathBuf};

pub struct SystemTokenFileEstimate {
    pub path: String,
    pub bytes: usize,
    pub tokens: usize,
}

pub struct SystemTokenEstimate {
    pub total_bytes: usize,
    pub total_tokens: usize,
    pub files: Vec<SystemTokenFileEstimate>,
}

fn list_markdown_files(dir: &Path, prefix: &str) -> Vec<String> {
    let target = if prefix.is_empty() {
        dir.to_path_buf()
    } else {
        dir.join(prefix)
    };
    let Ok(entries) = std::fs::read_dir(&target) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let relative = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}/{name}")
        };
        let Ok(file_type) = entry.file_type() else { continue };
        if file_type.is_dir() {
            files.extend(list_markdown_files(dir, &relative));
        } else if file_type.is_file() && name.ends_with(".md") {
            files.push(relative);
        }
    }
    files
}

pub fn estimate_system_tokens(repo_dir: &Path) -> SystemTokenEstimate {
    let paths = list_markdown_files(repo_dir, "system");
    let mut files: Vec<SystemTokenFileEstimate> = Vec::new();
    for relative in paths {
        let Ok(content) = std::fs::read_to_string(repo_dir.join(&relative)) else {
            continue;
        };
        let bytes = content.len();
        files.push(SystemTokenFileEstimate {
            path: relative,
            bytes,
            tokens: bytes.div_ceil(4),
        });
    }
    files.sort_by(|left, right| {
        right
            .tokens
            .cmp(&left.tokens)
            .then_with(|| left.path.cmp(&right.path))
    });
    let total_bytes = files.iter().map(|file| file.bytes).sum();
    let total_tokens = files.iter().map(|file| file.tokens).sum();
    SystemTokenEstimate { total_bytes, total_tokens, files }
}

/// Path of the system/ directory a repo's token estimate walks.
pub fn system_directory(repo_dir: &Path) -> PathBuf {
    repo_dir.join("system")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_repo() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp repo")
    }

    #[test]
    fn given_no_system_directory_when_estimated_then_the_total_is_zero() {
        let dir = temp_repo();
        let estimate = estimate_system_tokens(dir.path());
        assert_eq!(estimate.total_tokens, 0);
        assert!(estimate.files.is_empty());
    }

    #[test]
    fn given_system_files_including_nested_paths_when_estimated_then_tokens_are_bytes_per_four_sorted_by_size() {
        let dir = temp_repo();
        std::fs::create_dir_all(dir.path().join("system").join("projects")).expect("nested");
        std::fs::write(dir.path().join("system").join("persona.md"), "a".repeat(40)).expect("persona");
        std::fs::write(dir.path().join("system").join("projects").join("index.md"), "b".repeat(8))
            .expect("index");
        std::fs::create_dir_all(dir.path().join("external")).expect("external");
        std::fs::write(dir.path().join("external").join("ignored.md"), "c".repeat(400))
            .expect("ignored");

        let estimate = estimate_system_tokens(dir.path());

        let rows: Vec<(String, usize, usize)> = estimate
            .files
            .iter()
            .map(|file| (file.path.clone(), file.bytes, file.tokens))
            .collect();
        assert_eq!(
            rows,
            vec![
                ("system/persona.md".to_owned(), 40, 10),
                ("system/projects/index.md".to_owned(), 8, 2),
            ]
        );
        assert_eq!(estimate.total_bytes, 48);
        assert_eq!(estimate.total_tokens, 12);
    }

    #[test]
    fn given_a_file_whose_byte_length_is_not_divisible_by_four_when_estimated_then_the_file_rounds_up() {
        let dir = temp_repo();
        std::fs::create_dir_all(dir.path().join("system")).expect("system");
        std::fs::write(dir.path().join("system").join("odd.md"), "abcde").expect("odd");

        let estimate = estimate_system_tokens(dir.path());

        assert_eq!(estimate.files[0].tokens, 2);
        assert_eq!(estimate.total_tokens, 2);
    }
}
