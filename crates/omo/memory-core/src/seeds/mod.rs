//! Default memory repository seeds and initial template content.

pub mod default_memory;
pub mod memory_discipline;
#[expect(
    clippy::module_inception,
    reason = "mirrors the TS seeds/seeds.ts layout"
)]
pub mod seeds;

pub use default_memory::{
    DEFAULT_HUMAN_BODY, DEFAULT_MEMORY_BLOCK_LABELS, DEFAULT_PERSONA_BODY, V1_PERSONA_SEED_SHA256,
};
pub use memory_discipline::{MEMORY_DISCIPLINE_SKILL_CONTENT, MEMORY_DISCIPLINE_SKILL_PATH};
pub use seeds::{
    DefaultSeedFile, InitMemorySeedsOptions, InitializeGitRepoOptions, MemorySeedRepo,
    build_default_seed_files, init_memory_with_seeds,
};
