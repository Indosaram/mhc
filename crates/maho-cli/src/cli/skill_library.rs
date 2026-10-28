//! MIT profile (`--no-default-features`): skill support from `maho-skills`, mounted as
//! `<builtin:skills>`. The senpi skill loader in `maho-core` still discovers, validates and lists
//! skills; this adds the bundled skill root (`MAHO_SKILLS_ROOT`, else `skills/` beside the
//! executable) through `resources_discover`, the `disabled_skills` denylist from `omo.jsonc`, the
//! `skill` tool and the `/skill` command. The `omo` build does not mount it.

/// The skills extension with this process's bundled root.
pub fn extension() -> maho_skills::SkillsExtension {
    maho_skills::SkillsExtension::from_env()
}

/// The bundled skill root, for the app server's `skills/list`.
pub fn bundled_root() -> Option<std::path::PathBuf> {
    maho_skills::bundled_skills_root()
}
