use maho_omo_task::task_skill_loader::{create_task_skill_loader,TaskSkillLoaderOptions};
#[test]
fn resolved_plugin_directory_uses_existing_filesystem_loader() {
    let root=tempfile::tempdir().expect("root");
    let loader=create_task_skill_loader(TaskSkillLoaderOptions { home_dir:root.path().join("home"),agent_dir:root.path().join("agent"),plugin_skills_dirs:vec![std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/skills")],load_settings:None });
    let result=loader(&["sample".into(),"missing".into()],root.path().to_str().expect("path"));
    assert_eq!(result.resolved,["sample"]); assert_eq!(result.missing,["missing"]);
    let skills=result.skills.expect("loaded skills"); assert_eq!(skills[0].name,"sample"); assert!(skills[0].location.as_deref().expect("location").replace('\\', "/").ends_with("sample/SKILL.md"));
}
#[test]
fn canonical_agent_skill_wins_over_packaged_collision() {
    let root=tempfile::tempdir().expect("root"); let fixtures=std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures"); let canonical=fixtures.join("agent");
    let loader=create_task_skill_loader(TaskSkillLoaderOptions { home_dir:root.path().join("home"),agent_dir:canonical.clone(),plugin_skills_dirs:vec![fixtures.join("skills")],load_settings:None });
    let result=loader(&["sample".into()],root.path().to_str().expect("path")); assert_eq!(result.resolved,["sample"]); let skills=result.skills.expect("skills"); assert_eq!(std::path::Path::new(skills[0].location.as_deref().expect("location")),canonical.join("skills/sample/SKILL.md"));
}

#[test] fn existing_attribution_is_not_appended_twice() {
    use std::sync::Arc;
    let root = tempfile::tempdir().unwrap(); let skills = root.path().join("skills"); std::fs::create_dir_all(skills.join("git-master")).unwrap();
    std::fs::write(skills.join("git-master/SKILL.md"), "---\nname: git-master\ndescription: Git operations\n---\n<commit_attribution>existing</commit_attribution>\n").unwrap();
    let loader = create_task_skill_loader(TaskSkillLoaderOptions { home_dir: root.path().join("home"), agent_dir: root.path().join("agent"), plugin_skills_dirs: vec![skills], load_settings: Some(Arc::new(|_| maho_omo_git_master::GitMasterSettings::from_resolved(&serde_json::json!({"commit_footer":true,"git_env_prefix":""})))) });
    let result = loader(&["git-master".into()], root.path().to_str().unwrap());
    let content = &result.skills.unwrap()[0].content; assert_eq!(content.matches("<commit_attribution>").count(), 1);
}
