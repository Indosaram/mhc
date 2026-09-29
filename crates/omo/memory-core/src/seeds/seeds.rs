//! Default memory seeding logic and file generation.

use crate::memfs::frontmatter::{MemoryFrontmatter, render_memory_file};
use crate::seeds::default_memory::{DEFAULT_HUMAN_BODY, DEFAULT_PERSONA_BODY};
use crate::seeds::memory_discipline::{
    MEMORY_DISCIPLINE_SKILL_CONTENT, MEMORY_DISCIPLINE_SKILL_PATH,
};

pub use crate::seeds::default_memory::DEFAULT_MEMORY_BLOCK_LABELS;

/// A seed file prepared for repository initialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultSeedFile {
    pub relative_path: String,
    pub content: String,
}

/// Options passed when initializing memory seeds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InitMemorySeedsOptions {
    pub author_name: Option<String>,
}

/// Options passed to the underlying git repository init method.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InitializeGitRepoOptions {
    pub author_name: Option<String>,
    pub seed_files: Vec<DefaultSeedFile>,
}

/// Trait defining the repository initialization contract needed by memory seeding.
pub trait MemorySeedRepo {
    /// Initialize the repository with seed files and return the HEAD commit SHA.
    fn init(
        &self,
        options: InitializeGitRepoOptions,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>>;
}

/// Build the default seed files: persona, human, and memory discipline skill.
pub fn build_default_seed_files() -> Vec<DefaultSeedFile> {
    let persona_content = render_memory_file(
        &MemoryFrontmatter {
            description: "Persona - who I am".to_string(),
            read_only: None,
            kind: None,
            aliases: None,
        },
        DEFAULT_PERSONA_BODY,
    )
    .unwrap_or_default();

    let human_content = render_memory_file(
        &MemoryFrontmatter {
            description: "Person - Human".to_string(),
            read_only: None,
            kind: Some("person".to_string()),
            aliases: Some(Vec::new()),
        },
        DEFAULT_HUMAN_BODY,
    )
    .unwrap_or_default();

    vec![
        DefaultSeedFile {
            relative_path: "system/persona.md".to_string(),
            content: persona_content,
        },
        DefaultSeedFile {
            relative_path: "system/human.md".to_string(),
            content: human_content,
        },
        DefaultSeedFile {
            relative_path: MEMORY_DISCIPLINE_SKILL_PATH.to_string(),
            content: MEMORY_DISCIPLINE_SKILL_CONTENT.to_string(),
        },
    ]
}

/// Initialize a memory repo with default seed content.
pub fn init_memory_with_seeds<R: MemorySeedRepo>(
    repo: &R,
    options: InitMemorySeedsOptions,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    repo.init(InitializeGitRepoOptions {
        author_name: options.author_name,
        seed_files: build_default_seed_files(),
    })
}

#[cfg(test)]
#[path = "seeds_tests.rs"]
mod tests;
