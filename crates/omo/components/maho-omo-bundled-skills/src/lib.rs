//! Port of omo-senpi `components/bundled-skills` at latest `455dee623c9b2f2d2f1e9e68bf7b72a878ee3f0b`.
//!
//! One upstream source file per Rust module, names and order preserved:
//! `index.ts` -> [`index`], `contributed-skill.ts` -> [`contributed_skill`].

pub mod contributed_skill;
pub mod index;

pub use contributed_skill::{
    ContributedSkill, HARNESS, HostCommandInfo, HostCommands, ResolveContributedSkillOptions,
    host_command_info, host_commands_from_runtime, read_disabled_skills, read_discover_cwd,
    resolve_contributed_skill,
};
pub use index::{
    BUNDLED_SKILLS_COMPONENT_NAME, BUNDLED_SKILLS_ROOT_ENV, BundledSkillsComponent,
    BundledSkillsComponentOptions, resolve_bundled_skills_dir, resolve_bundled_skills_dir_from,
};
