//! Port of omo-senpi `components/skill-commands` at latest `455dee623c9b2f2d2f1e9e68bf7b72a878ee3f0b`.
//!
//! One upstream source file per Rust module, names and order preserved:
//! `index.ts` -> [`index`], `bare-skill-command.ts` -> [`bare_skill_command`],
//! `autocomplete.ts` -> [`autocomplete`].

pub mod autocomplete;
pub mod bare_skill_command;
pub mod index;

pub use autocomplete::{BareSkillCommandsProvider, wrap_with_bare_skill_commands};
pub use bare_skill_command::{
    BareSkillCommand, BareSkillCommandResolution, HostCommandInfo, SKILL_COMMAND_PREFIX,
    parse_bare_skill_command, read_bundled_skill_names, resolve_bare_skill_command,
};
pub use index::{SKILL_COMMANDS_COMPONENT_NAME, SkillCommandsComponent, SkillCommandsComponentOptions};
