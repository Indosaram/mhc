use maho_omo_task::task_skill_loader::{create_task_skill_loader,TaskSkillLoaderOptions};
#[test]
fn resolved_plugin_directory_uses_existing_filesystem_loader() {
    let root=tempfile::tempdir().expect("root");
    let loader=create_task_skill_loader(TaskSkillLoaderOptions { home_dir:root.path().join("home"),agent_dir:root.path().join("agent"),plugin_skills_dirs:vec![std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/skills")] });
    let result=loader(&["sample".into(),"missing".into()],root.path().to_str().expect("path"));
    assert_eq!(result.resolved,["sample"]); assert_eq!(result.missing,["missing"]);
    let skills=result.skills.expect("loaded skills"); assert_eq!(skills[0].name,"sample"); assert!(skills[0].location.as_deref().expect("location").ends_with("sample/SKILL.md"));
}
