use std::path::PathBuf;
use senpi_task::tools::task::{skills::{create_fs_skill_loader,FsSkillLoaderOptions},types::SkillLoader};
/// Resolved agent-home and package directories are supplied by their owning
/// adapter until those crates expose the default resolver contract.
pub struct TaskSkillLoaderOptions { pub agent_dir:PathBuf,pub home_dir:PathBuf,pub plugin_skills_dirs:Vec<PathBuf> }
pub fn create_task_skill_loader(options:TaskSkillLoaderOptions)->std::sync::Arc<SkillLoader> {
    create_fs_skill_loader(FsSkillLoaderOptions { home_dir:Some(options.home_dir),agent_dir:Some(options.agent_dir),extra_dirs:options.plugin_skills_dirs,..Default::default() })
}
