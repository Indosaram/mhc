use std::sync::Mutex;

use pretty_assertions::assert_eq;

use super::*;
use crate::memfs::frontmatter::parse_memory_file;

struct MockGitMemoryRepo {
    pub recorded_init_options: Mutex<Option<InitializeGitRepoOptions>>,
    pub return_sha: String,
}

impl MemorySeedRepo for MockGitMemoryRepo {
    fn init(
        &self,
        options: InitializeGitRepoOptions,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        *self.recorded_init_options.lock().unwrap() = Some(options);
        Ok(self.return_sha.clone())
    }
}

#[test]
fn test_build_default_seed_files_when_called_then_produces_three_files_with_expected_paths_and_frontmatter()
 {
    let files = build_default_seed_files();
    assert_eq!(files.len(), 3);

    let paths: Vec<String> = files.iter().map(|f| f.relative_path.clone()).collect();
    assert!(paths.contains(&"system/persona.md".to_string()));
    assert!(paths.contains(&"system/human.md".to_string()));
    assert!(paths.contains(&"skills/memory-discipline/SKILL.md".to_string()));

    for file in &files {
        let parsed = parse_memory_file(&file.content);
        assert!(
            parsed.is_ok(),
            "file {} failed to parse: {:?}",
            file.relative_path,
            parsed.err()
        );
    }

    let human = files
        .iter()
        .find(|f| f.relative_path == "system/human.md")
        .unwrap();
    let parsed_human = parse_memory_file(&human.content).unwrap();
    assert_eq!(parsed_human.frontmatter.kind, Some("person".to_string()));
    assert_eq!(parsed_human.frontmatter.aliases, Some(Vec::new()));
}

#[test]
fn test_init_memory_with_seeds_when_called_on_fresh_repo_then_commits_initial_seed_files() {
    let repo = MockGitMemoryRepo {
        recorded_init_options: Mutex::new(None),
        return_sha: "0123456789abcdef0123456789abcdef01234567".to_string(),
    };

    let result = init_memory_with_seeds(
        &repo,
        InitMemorySeedsOptions {
            author_name: Some("Seed Test Agent".to_string()),
        },
    )
    .unwrap();

    assert_eq!(result, "0123456789abcdef0123456789abcdef01234567");

    let recorded = repo.recorded_init_options.lock().unwrap().take().unwrap();
    assert_eq!(recorded.author_name, Some("Seed Test Agent".to_string()));
    assert_eq!(recorded.seed_files.len(), 3);
}

#[test]
fn test_init_memory_with_seeds_when_called_without_author_name_then_uses_none_author() {
    let repo = MockGitMemoryRepo {
        recorded_init_options: Mutex::new(None),
        return_sha: "abcdef0123456789abcdef0123456789abcdef01".to_string(),
    };

    let result = init_memory_with_seeds(&repo, InitMemorySeedsOptions::default()).unwrap();

    assert_eq!(result, "abcdef0123456789abcdef0123456789abcdef01");

    let recorded = repo.recorded_init_options.lock().unwrap().take().unwrap();
    assert_eq!(recorded.author_name, None);
    assert_eq!(recorded.seed_files.len(), 3);
}

#[test]
fn test_init_memory_with_seeds_when_called_on_existing_repo_then_returns_head_unchanged() {
    let existing_head = "fedcba9876543210fedcba9876543210fedcba98".to_string();
    let repo = MockGitMemoryRepo {
        recorded_init_options: Mutex::new(None),
        return_sha: existing_head.clone(),
    };

    let result = init_memory_with_seeds(
        &repo,
        InitMemorySeedsOptions {
            author_name: Some("Second Agent".to_string()),
        },
    )
    .unwrap();

    assert_eq!(result, existing_head);
}
