mod support;

use std::collections::BTreeMap;
use std::path::Path;

use maho_ext_api::*;
use maho_ext_host::ExtensionRunner;
use maho_omo_bundled_skills::{
    BUNDLED_SKILLS_ROOT_ENV, BundledSkillsComponent, BundledSkillsComponentOptions,
    resolve_bundled_skills_dir_from,
};

fn component(skills_dir: Option<&Path>, env: &BTreeMap<String, String>) -> BundledSkillsComponent {
    BundledSkillsComponent::new(BundledSkillsComponentOptions {
        env: Some(env.clone()),
        skills_dir: skills_dir.map(Path::to_path_buf),
    })
}

fn fixture_skills(root: &Path) -> std::path::PathBuf {
    let skills = root.join("skills");
    support::write_skill(&skills, "beta");
    support::write_skill(&skills, "alpha");
    std::fs::create_dir_all(skills.join("not-a-skill")).expect("plain directory");
    std::fs::write(skills.join("notes.txt"), "ignored").expect("plain file");
    skills
}

fn fixture_home(root: &Path) -> std::path::PathBuf {
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("home");
    home
}

fn write_disabled_skills(home: &Path, names: &[&str]) {
    let dir = home.join(".maho");
    std::fs::create_dir_all(&dir).expect("config dir");
    let list: Vec<String> = names.iter().map(|name| format!("\"{name}\"")).collect();
    std::fs::write(dir.join("omo.jsonc"), format!("{{\"disabled_skills\":[{}]}}", list.join(","))).expect("config file");
}

#[tokio::test]
async fn given_a_bundled_skills_directory_when_discovery_runs_then_every_skill_md_is_contributed_with_system_scope() {
    let root = tempfile::tempdir().expect("temp");
    let skills = fixture_skills(root.path());
    let home = fixture_home(root.path());
    let env = support::env_for_home(&home);

    let mut runner = ExtensionRunner::from_static(vec![Box::new(component(Some(&skills), &env))], support::context(&home, None));
    let discovered = runner.emit_resources_discover(home.clone(), SessionReason::Startup).await.expect("discover");

    let expected: Vec<String> = ["alpha", "beta"]
        .iter()
        .map(|name| skills.join(name).join("SKILL.md").to_string_lossy().into_owned())
        .collect();
    let actual: Vec<String> = discovered.skill_paths.iter().map(|entry| entry.path.clone()).collect();
    assert_eq!(actual, expected);
    assert!(discovered.skill_paths.iter().all(|entry| entry.scope == Some(SourceScope::System)));
    assert!(discovered.prompt_paths.is_empty() && discovered.theme_paths.is_empty() && discovered.hook_paths.is_empty());
}

#[tokio::test]
async fn given_disabled_skills_names_a_bundled_skill_when_discovery_runs_then_it_is_absent_and_reported() {
    let root = tempfile::tempdir().expect("temp");
    let skills = fixture_skills(root.path());
    let home = fixture_home(root.path());
    write_disabled_skills(&home, &["beta"]);
    let env = support::env_for_home(&home);
    let logger = std::sync::Arc::new(support::RecordingLogger::default());

    let mut runner = ExtensionRunner::from_static(
        vec![Box::new(component(Some(&skills), &env))],
        support::context(&home, Some(logger.clone())),
    );
    let discovered = runner.emit_resources_discover(home.clone(), SessionReason::Startup).await.expect("discover");

    let actual: Vec<String> = discovered.skill_paths.iter().map(|entry| entry.path.clone()).collect();
    assert_eq!(actual, vec![skills.join("alpha").join("SKILL.md").to_string_lossy().into_owned()]);

    let messages = logger.messages();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].0, "bundled skills hidden by disabled_skills");
    assert_eq!(
        messages[0].1,
        Some(serde_json::json!({"component": "bundled-skills", "hidden": ["beta"]}))
    );
}

#[tokio::test]
async fn given_an_edited_denylist_when_a_reload_discovers_again_then_the_new_config_is_read() {
    let root = tempfile::tempdir().expect("temp");
    let skills = fixture_skills(root.path());
    let home = fixture_home(root.path());
    write_disabled_skills(&home, &["beta"]);
    let env = support::env_for_home(&home);

    let mut runner = ExtensionRunner::from_static(vec![Box::new(component(Some(&skills), &env))], support::context(&home, None));
    let first = runner.emit_resources_discover(home.clone(), SessionReason::Startup).await.expect("discover");
    assert_eq!(first.skill_paths.len(), 1);

    write_disabled_skills(&home, &["alpha"]);
    let second = runner.emit_resources_discover(home.clone(), SessionReason::Reload).await.expect("discover");

    let actual: Vec<String> = second.skill_paths.iter().map(|entry| entry.path.clone()).collect();
    assert_eq!(actual, vec![skills.join("beta").join("SKILL.md").to_string_lossy().into_owned()]);
}

#[tokio::test]
async fn given_a_missing_skills_directory_when_discovery_runs_then_nothing_is_contributed_and_the_error_is_reported() {
    let root = tempfile::tempdir().expect("temp");
    let home = fixture_home(root.path());
    let env = support::env_for_home(&home);
    let missing = root.path().join("no-such-skills");

    let mut runner = ExtensionRunner::from_static(vec![Box::new(component(Some(&missing), &env))], support::context(&home, None));
    let discovered = runner.emit_resources_discover(home.clone(), SessionReason::Startup).await.expect("discover");

    assert!(discovered.skill_paths.is_empty());
    assert_eq!(runner.errors.len(), 1);
    assert_eq!(runner.errors[0].event, "resources_discover");
}

#[test]
fn given_packaged_and_source_copies_when_the_skills_root_resolves_then_the_packaged_copy_wins() {
    let root = tempfile::tempdir().expect("temp");
    let importer = root.path().join("plugin").join("extensions");
    std::fs::create_dir_all(importer.join("skills")).expect("packaged skills");
    std::fs::create_dir_all(importer.join("plugin").join("skills")).expect("dev skills");

    let env = BTreeMap::new();
    assert_eq!(resolve_bundled_skills_dir_from(&env, Some(&importer)), Some(importer.join("skills")));
}

#[test]
fn given_only_the_dev_copy_when_the_skills_root_resolves_then_it_is_used() {
    let root = tempfile::tempdir().expect("temp");
    let importer = root.path().join("components").join("bundled-skills");
    std::fs::create_dir_all(&importer).expect("importer");
    std::fs::create_dir_all(importer.join("plugin").join("skills")).expect("dev skills");

    assert_eq!(
        resolve_bundled_skills_dir_from(&BTreeMap::new(), Some(&importer)),
        Some(importer.join("plugin").join("skills"))
    );
}

#[test]
fn given_an_env_skills_root_when_it_exists_then_it_outranks_the_packaged_candidates() {
    let root = tempfile::tempdir().expect("temp");
    let importer = root.path().join("extensions");
    std::fs::create_dir_all(importer.join("skills")).expect("packaged skills");
    let env_root = root.path().join("installed-skills");
    std::fs::create_dir_all(&env_root).expect("env skills");

    let env = BTreeMap::from([(BUNDLED_SKILLS_ROOT_ENV.to_owned(), env_root.to_string_lossy().into_owned())]);
    assert_eq!(resolve_bundled_skills_dir_from(&env, Some(&importer)), Some(env_root));
}

#[test]
fn given_a_set_env_skills_root_that_is_missing_when_resolved_then_it_stays_authoritative_over_a_packaged_candidate() {
    let root = tempfile::tempdir().expect("temp");
    let importer = root.path().join("extensions");
    std::fs::create_dir_all(importer.join("skills")).expect("packaged skills");
    let env_root = root.path().join("not-yet-staged");

    let env = BTreeMap::from([(BUNDLED_SKILLS_ROOT_ENV.to_owned(), env_root.to_string_lossy().into_owned())]);
    assert_eq!(resolve_bundled_skills_dir_from(&env, Some(&importer)), Some(env_root));
}

#[test]
fn given_no_candidate_exists_when_the_skills_root_resolves_then_it_is_absent() {
    let root = tempfile::tempdir().expect("temp");
    let importer = root.path().join("extensions");
    std::fs::create_dir_all(&importer).expect("importer");

    assert_eq!(resolve_bundled_skills_dir_from(&BTreeMap::new(), Some(&importer)), None);
}
