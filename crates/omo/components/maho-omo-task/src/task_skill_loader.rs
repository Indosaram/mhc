use std::path::{Path, PathBuf};
use std::sync::Arc;

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
    pub plugin_skills_dirs: Vec<PathBuf>,
    /// Upstream `loadSettings`; `None` reads the merged omo config for the cwd.
    pub load_settings: Option<LoadGitMasterSettings>,
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
                    Some(directive) if !skill.content.contains("<commit_attribution>") => format!("{content}\n\n{directive}"),
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
        extra_dirs: options.plugin_skills_dirs,
        ..Default::default()
    });
    with_git_master_settings(loader, load_settings)
}
