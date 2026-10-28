//! MIT-profile skill support (`docs/cleanroom/specs/skills.md`).
//!
//! Discovery, front matter validation, the `<available_skills>` prompt listing and `/skill:<name>`
//! expansion come from the senpi skill loader already ported into `maho-core`. This crate adds
//! what the MIT profile lacked once the omo glue was compiled out:
//!
//! - the bundled skill root ([`bundled_skills_root`]), contributed through `resources_discover`;
//! - the `disabled_skills` denylist from `omo.jsonc` ([`disabled_skills`], [`filter_prompt`]);
//! - the `skill` tool ([`run_skill_tool`]), which loads a skill or a file inside it, with the
//!   path rules of oh-my-pi's `skill://` protocol;
//! - the `/skill` command ([`list_text`], [`detail_text`]).
//!
//! See `PROVENANCE.md` for the MIT sources.

mod extension;
mod prompt;
mod root;
mod tool;

pub use extension::SkillsExtension;
pub use prompt::filter_prompt;
pub use root::{SKILLS_ROOT_ENV, bundled_skills_root, bundled_skills_root_from};
pub use tool::{
    MAX_FILE_BYTES, SkillToolOutcome, TOOL_NAME, detail_text, list_text, run_skill_tool,
    strip_front_matter,
};

/// The `disabled_skills` denylist for `cwd`: the union over every `omo.jsonc` layer
/// (`maho_config::collect_disabled_skills`, harness `native`, active profile).
pub fn disabled_skills(cwd: &std::path::Path) -> Vec<String> {
    let loaded = maho_config::load_native(cwd, &maho_config::process_env());
    maho_config::collect_disabled_skills(
        &loaded.layers,
        &maho_config::Harness::Native,
        loaded.profile.as_deref(),
    )
}
