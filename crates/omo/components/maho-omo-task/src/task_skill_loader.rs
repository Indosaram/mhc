use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use maho_ext_api::ComponentLogger;
use maho_omo_bundled_skills::resolve_bundled_skills_override;
use maho_omo_git_master::GitMasterSettings;
use maho_omo_git_master::skill_content::resolve_git_master_skill_additions;
use senpi_task::tools::task::skills::{
    FsSkillLoaderOptions, build_skill_prepend, create_fs_skill_loader,
};
use senpi_task::tools::task::types::{SkillLoader, SkillResolution};

const GIT_MASTER_SKILL_NAME: &str = "git-master";

pub type LoadGitMasterSettings = Arc<dyn Fn(&Path) -> GitMasterSettings + Send + Sync>;

/// Resolved agent-home and package directories are supplied by their owning
/// adapter until those crates expose the default resolver contract.
pub struct TaskSkillLoaderOptions {
    pub agent_dir: PathBuf,
    pub home_dir: PathBuf,
    /// Upstream `pluginSkillsDirs`; `None` resolves [`default_packaged_skill_dirs`] from `env`.
    pub plugin_skills_dirs: Option<Vec<PathBuf>>,
    /// Upstream `env ?? process.env`; read only when `plugin_skills_dirs` is `None`.
    pub env: Option<BTreeMap<String, String>>,
    /// Upstream `logger`: reports an invalid `OMO_BUNDLED_SKILLS_DIR` once.
    pub logger: Option<Arc<dyn ComponentLogger>>,
    /// Upstream `loadSettings`; `None` reads the merged omo config for the cwd.
    pub load_settings: Option<LoadGitMasterSettings>,
}

/// Upstream `packagedSkillDirs(moduleUrl, env, logger)`: a valid `OMO_BUNDLED_SKILLS_DIR` replaces the
/// packaged bundled-skills roots (an invalid one is reported once and ignored); the credential-gated
/// `skills-conditional` roots always follow. Only existing directories are kept.
pub fn packaged_skill_dirs(
    bundled: Vec<PathBuf>,
    conditional: Vec<PathBuf>,
    env: &BTreeMap<String, String>,
    logger: Option<&dyn ComponentLogger>,
) -> Vec<PathBuf> {
    let bundled = match resolve_bundled_skills_override(env, logger, None) {
        Some(override_dir) => vec![override_dir],
        None => bundled,
    };
    bundled
        .into_iter()
        .chain(conditional)
        .filter(|dir| dir.exists())
        .collect()
}

/// Upstream `packagedSkillDirs(import.meta.url, env, logger)` for the native layout: the packaged
/// skills root (`OMO_SENPI_SKILLS_ROOT`, else `OMO_SENPI_PLUGIN_ROOT/skills`, else `skills/` beside
/// the executable) and the `skills-conditional` roots beside the plugin root and the executable.
pub fn default_packaged_skill_dirs(
    env: &BTreeMap<String, String>,
    logger: Option<&dyn ComponentLogger>,
) -> Vec<PathBuf> {
    let plugin_root = env
        .get("OMO_SENPI_PLUGIN_ROOT")
        .filter(|root| !root.is_empty())
        .map(PathBuf::from);
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    let bundled = env
        .get(maho_omo_bundled_skills::index::BUNDLED_SKILLS_ROOT_ENV)
        .filter(|root| !root.is_empty())
        .map(PathBuf::from)
        .or_else(|| plugin_root.as_ref().map(|root| root.join("skills")))
        .or_else(|| exe_dir.as_ref().map(|dir| dir.join("skills")))
        .into_iter()
        .collect();
    let conditional = plugin_root
        .iter()
        .map(|root| root.join("skills-conditional"))
        .chain(exe_dir.iter().flat_map(|dir| {
            [
                dir.join("skills-conditional"),
                dir.join("plugin").join("skills-conditional"),
            ]
        }))
        .collect();
    packaged_skill_dirs(bundled, conditional, env, logger)
}

fn with_git_master_settings(
    loader: Arc<SkillLoader>,
    load_settings: LoadGitMasterSettings,
) -> Arc<SkillLoader> {
    Arc::new(move |names: &[String], cwd: &str| -> SkillResolution {
        let resolution = loader(names, cwd);
        let Some(resolved_skills) = resolution.skills.as_ref() else {
            return resolution;
        };
        if !resolved_skills
            .iter()
            .any(|skill| skill.name == GIT_MASTER_SKILL_NAME)
        {
            return resolution;
        }
        let Some(additions) = resolve_git_master_skill_additions(&load_settings(Path::new(cwd)))
        else {
            return resolution;
        };
        let skills: Vec<_> = resolved_skills
            .iter()
            .map(|skill| {
                if skill.name != GIT_MASTER_SKILL_NAME {
                    return skill.clone();
                }
                let content = (additions.rewrite_skill_text)(&skill.content);
                let content = match &additions.directive {
                    None => content,
                    Some(directive) if !skill.content.contains("<commit_attribution>") => {
                        format!("{content}\n\n{directive}")
                    }
                    Some(_) => content,
                };
                let mut skill = skill.clone();
                skill.content = content;
                skill
            })
            .collect();
        SkillResolution {
            prepend: build_skill_prepend(&skills, ""),
            skills: Some(skills),
            ..resolution
        }
    })
}

pub fn create_task_skill_loader(options: TaskSkillLoaderOptions) -> Arc<SkillLoader> {
    let load_settings = options.load_settings.unwrap_or_else(|| {
        Arc::new(|cwd: &Path| maho_omo_git_master::default_load_settings(cwd, None))
    });
    let loader = create_fs_skill_loader(FsSkillLoaderOptions {
        home_dir: Some(options.home_dir),
        agent_dir: Some(options.agent_dir),
        extra_dirs: options.plugin_skills_dirs.unwrap_or_else(|| {
            let env = options.env.unwrap_or_else(|| std::env::vars().collect());
            default_packaged_skill_dirs(&env, options.logger.as_deref())
        }),
        ..Default::default()
    });
    with_git_master_settings(loader, load_settings)
}
