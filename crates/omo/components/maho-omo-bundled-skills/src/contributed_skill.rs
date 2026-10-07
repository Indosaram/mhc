//! Port of `omo-senpi/src/components/bundled-skills/contributed-skill.ts` at latest
//! `455dee623c9b2f2d2f1e9e68bf7b72a878ee3f0b`.
//!
//! `readDisabledSkills` reads the `disabled_skills` union through the config loader on every call.
//! The native union collector (`collect_disabled_skills`) is owned by the config lane (contract
//! U-L8, `crates/omo/omo-config-core/src/loader/disabled_skills.rs`); this module consumes it and
//! implements no local fallback.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use maho_ext_api::{ExtensionFailure, ExtensionRuntime, ResourcesDiscoverEvent, SlashCommandInfo};
use omo_config_core::{
    CollectDisabledSkillsOptions, LoadOmoConfigOptions, collect_disabled_skills, load_omo_config,
};

/// The harness spelling upstream passes to the denylist collector: the legacy id, not the canonical
/// one, so the collector expands it to the canonical block plus every alias (upstream
/// `OMO_CONFIG_LEGACY_HARNESS_ALIASES = { senpi: "native" }` -> keys `[native]` and `[senpi]`).
pub const HARNESS: &str = "senpi";

/// Upstream `pi.getCommands()`: one entry per registered slash command. Optional as a whole —
/// `None` means the host predates the API, exactly like upstream's `getCommands === undefined`.
pub type HostCommands = Arc<dyn Fn() -> Result<Option<Vec<HostCommandInfo>>, ExtensionFailure> + Send + Sync>;

/// The `getCommands()` entry fields both components read: `name`, `description`, `source` and
/// `sourceInfo.path`. Upstream declares the narrow `HostCommandInfo` in
/// `skill-commands/bare-skill-command.ts` and the wider `getCommands()` shape inline in
/// `contributed-skill.ts`; both are projections of the same native `SlashCommandInfo`, and the
/// crate dependency direction (`skill-commands` -> `bundled-skills`) puts the shared type here.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostCommandInfo {
    pub name: String,
    pub description: Option<String>,
    pub source: String,
    pub source_info_path: Option<String>,
}

/// Upstream `pi.getCommands?.()` reached through the runtime the component captured at
/// registration: the native `ExtensionContext` exposes no command registry, the runtime does.
pub fn host_commands_from_runtime(runtime: ExtensionRuntime) -> HostCommands {
    Arc::new(move || match runtime.session_actions() {
        Ok(actions) => actions
            .get_commands()
            .map(|commands| Some(commands.iter().map(host_command_info).collect())),
        Err(_) => Ok(None),
    })
}

pub fn host_command_info(command: &SlashCommandInfo) -> HostCommandInfo {
    HostCommandInfo {
        name: command.name.clone(),
        description: command.description.clone(),
        source: command.source_info.as_ref().map(|info| info.source.clone()).unwrap_or_default(),
        source_info_path: command.source_info.as_ref().map(|info| info.path.clone()),
    }
}

pub fn read_disabled_skills(cwd: &Path, env: &BTreeMap<String, String>) -> BTreeSet<String> {
    let loaded = load_omo_config(&LoadOmoConfigOptions {
        cwd: Some(cwd.to_string_lossy().into_owned()),
        env: Some(env.clone()),
        ..Default::default()
    });
    collect_disabled_skills(&CollectDisabledSkillsOptions {
        harness: Some(HARNESS),
        layers: &loaded.layers,
        profile: loaded.profile.as_deref(),
    })
    .into_iter()
    .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContributedSkill {
    Contributed { path: PathBuf },
    Disabled,
    Yielded { owner_path: Option<String> },
}

pub struct ResolveContributedSkillOptions<'a> {
    pub commands: Option<&'a [HostCommandInfo]>,
    pub name: &'a str,
    pub path: PathBuf,
    pub cwd: PathBuf,
    pub env: &'a BTreeMap<String, String>,
}

/// One `resources_discover` pass for a skill an omo component contributes on its own (computer-use,
/// x-search), as opposed to the bundled skills directory.
///
/// senpi appends extension skill paths after the user, project, settings and package skills it has
/// already loaded, keeps the first skill of each name, and reports every later one as a "Skill
/// conflicts" collision. A same-name skill that is already loaded wins either way, so ours yields
/// without a word. Our own copy left over from an earlier pass is not a rival. `disabled_skills`
/// hides the skill exactly as it hides a bundled one.
pub fn resolve_contributed_skill(options: ResolveContributedSkillOptions<'_>) -> ContributedSkill {
    if read_disabled_skills(&options.cwd, options.env).contains(options.name) {
        return ContributedSkill::Disabled;
    }
    let command = format!("skill:{}", options.name);
    let loaded = options
        .commands
        .and_then(|commands| commands.iter().find(|entry| entry.source == "skill" && entry.name == command));
    let path = options.path;
    let Some(loaded) = loaded else { return ContributedSkill::Contributed { path } };
    match &loaded.source_info_path {
        Some(owner_path) if canonical_path(Path::new(owner_path)) == canonical_path(&path) => {
            ContributedSkill::Contributed { path }
        }
        owner_path => ContributedSkill::Yielded { owner_path: owner_path.clone() },
    }
}

/// Upstream `readDiscoverCwd(payload)`: the discover pass carries the cwd it runs for; a host that
/// reports none falls back to `pi.cwd`.
pub fn read_discover_cwd(event: &ResourcesDiscoverEvent) -> Option<PathBuf> {
    if event.cwd.as_os_str().is_empty() { None } else { Some(event.cwd.clone()) }
}

fn canonical_path(path: &Path) -> PathBuf {
    if path.exists() {
        std::fs::canonicalize(path).unwrap_or_else(|_| absolute_path(path))
    } else {
        absolute_path(path)
    }
}

fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map(|cwd| cwd.join(path)).unwrap_or_else(|_| path.to_path_buf())
    }
}
