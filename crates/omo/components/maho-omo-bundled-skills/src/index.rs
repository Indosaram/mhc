//! Port of `omo-senpi/src/components/bundled-skills/index.ts` at latest
//! `455dee623c9b2f2d2f1e9e68bf7b72a878ee3f0b`.
//!
//! Contributes the plugin's bundled skills through `resources_discover`, minus `disabled_skills`.
//! The plugin manifest deliberately declares no `pi.skills`: the launcher always loads the plugin
//! with `--extension <plugin>`, and senpi applies no user-level filter to a command-line package's
//! manifest skills, so a manifest entry would put every bundled skill into every run regardless of
//! config. Contributing them here lets `disabled_skills` make a skill absent from the session: it
//! never reaches the `<available_skills>` index, the `/skill:` commands, or `get_commands`.
//!
//! The plugin is a system package, so paths inside its root keep the `system` scope senpi assigns
//! to the manifest entries this replaces. Config is re-read on every discover pass, so `/reload`
//! picks up an edited denylist.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use maho_ext_api::{
    ComponentLogger, EventKind, EventResult, Extension, ExtensionApi, ExtensionContext,
    ExtensionEvent, ExtensionFailure, ResourceDiscoverEntry, ResourcesDiscoverResult, SourceScope,
};

use crate::contributed_skill::{read_disabled_skills, read_discover_cwd};

pub const BUNDLED_SKILLS_COMPONENT_NAME: &str = "bundled-skills";

/// The native packaged-skills root contract, shared with `maho_omo::component_list::builtin_skills_root`
/// and `maho-cli/src/cli/omo_mount.rs` (upstream `getBuiltinSkillsRoot()`).
pub const BUNDLED_SKILLS_ROOT_ENV: &str = "OMO_SENPI_SKILLS_ROOT";

#[derive(Clone, Debug, Default)]
pub struct BundledSkillsComponentOptions {
    /// Upstream `options.env ?? process.env`.
    pub env: Option<BTreeMap<String, String>>,
    /// Overrides the packaged skills root; tests point it at a fixture.
    pub skills_dir: Option<PathBuf>,
}

pub struct BundledSkillsComponent {
    pub options: BundledSkillsComponentOptions,
}

impl BundledSkillsComponent {
    pub const NAME: &'static str = BUNDLED_SKILLS_COMPONENT_NAME;

    pub fn new(options: BundledSkillsComponentOptions) -> Self {
        Self { options }
    }

    pub fn from_env(env: &BTreeMap<String, String>) -> Self {
        Self { options: BundledSkillsComponentOptions { env: Some(env.clone()), skills_dir: None } }
    }
}

impl Extension for BundledSkillsComponent {
    fn register(&self, api: &mut ExtensionApi) {
        let options = self.options.clone();
        api.on(
            EventKind::ResourcesDiscover,
            Arc::new(move |event, ctx| {
                let result = resources_discover(event, ctx, &options);
                Box::pin(async move { result })
            }),
        );
    }
}

fn resources_discover(
    event: &ExtensionEvent,
    ctx: &ExtensionContext,
    options: &BundledSkillsComponentOptions,
) -> Result<EventResult, ExtensionFailure> {
    let ExtensionEvent::ResourcesDiscover(discover) = event else { return Ok(EventResult::None) };
    let env = options.env.clone().unwrap_or_else(process_env);
    let Some(skills_dir) = options.skills_dir.clone().or_else(|| resolve_bundled_skills_dir(&env)) else {
        return Ok(EventResult::None);
    };
    let cwd = read_discover_cwd(discover).unwrap_or_else(|| ctx.cwd.clone());
    collect_skill_paths(&skills_dir, &cwd, &env, ctx.logger.as_deref())
        .map(EventResult::ResourcesDiscover)
}

fn collect_skill_paths(
    skills_dir: &Path,
    cwd: &Path,
    env: &BTreeMap<String, String>,
    logger: Option<&dyn ComponentLogger>,
) -> Result<ResourcesDiscoverResult, ExtensionFailure> {
    let disabled = read_disabled_skills(cwd, env);
    let mut skill_paths = Vec::new();
    let mut hidden = Vec::new();
    for name in read_sorted_names(skills_dir).map_err(|error| {
        ExtensionFailure::new(format!("bundled skills directory {}: {error}", skills_dir.display()))
    })? {
        let skill_file = skills_dir.join(&name).join("SKILL.md");
        if !skill_file.exists() {
            continue;
        }
        if disabled.contains(&name) {
            hidden.push(name);
            continue;
        }
        skill_paths.push(ResourceDiscoverEntry {
            path: skill_file.to_string_lossy().into_owned(),
            scope: Some(SourceScope::System),
        });
    }
    if !hidden.is_empty()
        && let Some(logger) = logger
    {
        logger.info(
            "bundled skills hidden by disabled_skills",
            Some(&serde_json::json!({ "component": BUNDLED_SKILLS_COMPONENT_NAME, "hidden": hidden })),
        );
    }
    Ok(ResourcesDiscoverResult { skill_paths, ..Default::default() })
}

/// Upstream `readdirSync(skillsDir).sort()`: the directory's entries in a stable order.
fn read_sorted_names(dir: &Path) -> std::io::Result<Vec<String>> {
    let mut names: Vec<String> = std::fs::read_dir(dir)?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<std::io::Result<Vec<String>>>()?;
    names.sort();
    Ok(names)
}

/// Packaged plugin skills win; the source-tree copy keeps dev runs working. From the bundled
/// extension the packaged candidate resolves first; from the source file the second candidate
/// resolves to the same synced directory.
pub fn resolve_bundled_skills_dir(env: &BTreeMap<String, String>) -> Option<PathBuf> {
    resolve_bundled_skills_dir_from(env, executable_dir().as_deref())
}

/// Upstream `resolveBundledSkillsDir(importerUrl = import.meta.url)`: the importer's directory is
/// the native executable's directory (the analogue of the bundled extension's own URL).
pub fn resolve_bundled_skills_dir_from(env: &BTreeMap<String, String>, importer_dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(root) = env.get(BUNDLED_SKILLS_ROOT_ENV).filter(|root| !root.is_empty()) {
        return Some(PathBuf::from(root));
    }
    bundled_skills_dir_candidates(env, importer_dir).into_iter().find(|candidate| candidate.exists())
}

/// `OMO_SENPI_SKILLS_ROOT` is the native packaged-root contract (`maho_omo::component_list::skills_root_from`,
/// `maho-cli/src/cli/omo_mount.rs`): a set, non-empty value IS the session's skills root and wins
/// unconditionally, so a misconfigured root is reported by the discover pass instead of silently
/// contributing from a different directory. Only when it is unset do upstream's two
/// importer-relative candidates apply, with the first existing one winning.
fn bundled_skills_dir_candidates(env: &BTreeMap<String, String>, importer_dir: Option<&Path>) -> Vec<PathBuf> {
    if let Some(root) = env.get(BUNDLED_SKILLS_ROOT_ENV).filter(|root| !root.is_empty()) {
        return vec![PathBuf::from(root)];
    }
    let mut candidates = Vec::new();
    if let Some(dir) = importer_dir {
        candidates.push(dir.join("skills"));
        candidates.push(dir.join("plugin").join("skills"));
    }
    candidates
}

fn executable_dir() -> Option<PathBuf> {
    std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf))
}

fn process_env() -> BTreeMap<String, String> {
    std::env::vars().collect()
}
