//! §8.11 skill contribution.

use crate::availability::{env_of, extension};
use crate::support;
use maho_ext_api::{
    EventKind, EventResult, Extension, ExtensionEvent, ResourcesDiscoverEvent, SessionReason,
    SourceScope,
};
use maho_x_search::XSearchExtension;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

fn connected(agent_dir: &Path) -> XSearchExtension {
    extension(agent_dir, env_of(&[("XAI_API_KEY", "k")]))
}

fn skill_file(root: &Path) -> PathBuf {
    let file = root.join("bundled").join("x-search").join("SKILL.md");
    std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
    std::fs::write(&file, maho_x_search::skill::SKILL_TEXT).expect("write");
    file
}

/// Registers `extension` and runs its `resources_discover` handler for `cwd`.
async fn discover(
    extension: &XSearchExtension,
    cwd: &Path,
) -> Option<Vec<(String, Option<SourceScope>)>> {
    let mut api = support::api(cwd);
    extension.register(&mut api);
    let handler = api
        .registered
        .handlers
        .get(&EventKind::ResourcesDiscover)?
        .first()?
        .clone();
    let ctx = support::context(cwd, crate::support::FakeRegistry::new(None, Ok(None)));
    let mut event = ExtensionEvent::ResourcesDiscover(ResourcesDiscoverEvent {
        cwd: cwd.to_path_buf(),
        reason: SessionReason::Startup,
        scoped_entries: false,
    });
    match handler(&mut event, &ctx).await.expect("handler") {
        EventResult::ResourcesDiscover(result) => Some(
            result
                .skill_paths
                .into_iter()
                .map(|entry| (entry.path, entry.scope))
                .collect(),
        ),
        _ => Some(Vec::new()),
    }
}

#[tokio::test]
async fn contributed_as_a_system_skill_when_connected() {
    let dir = tempfile::tempdir().expect("dir");
    let mut extension = connected(dir.path());
    let file = skill_file(dir.path());
    extension.skill_file = Some(file.clone());
    let paths = discover(&extension, dir.path())
        .await
        .expect("handler registered");
    assert_eq!(
        paths,
        [(
            file.to_string_lossy().into_owned(),
            Some(SourceScope::System)
        )]
    );
}

#[tokio::test]
async fn embedded_skill_is_used_by_default() {
    let dir = tempfile::tempdir().expect("dir");
    let paths = discover(&connected(dir.path()), dir.path())
        .await
        .expect("handler registered");
    assert_eq!(paths.len(), 1);
    let text = std::fs::read_to_string(&paths[0].0).expect("skill file");
    assert!(text.starts_with("---\nname: x-search\n"));
}

#[tokio::test]
async fn disabled_skills_hides_it() {
    let dir = tempfile::tempdir().expect("dir");
    let mut extension = connected(dir.path());
    extension.skill_file = Some(skill_file(dir.path()));
    extension.disabled_skills = Arc::new(|_: &Path| vec!["other".into(), "x-search".into()]);
    assert_eq!(discover(&extension, dir.path()).await, Some(Vec::new()));
}

#[test]
fn disabled_skills_reads_the_project_omo_config() {
    let dir = tempfile::tempdir().expect("dir");
    let project = dir.path().join("project");
    std::fs::create_dir_all(project.join(".omo")).expect("mkdir");
    std::fs::write(
        project.join(".omo/omo.jsonc"),
        r#"{"disabled_skills": ["x-search"]}"#,
    )
    .expect("write");
    let env: maho_config::Env = [(
        "HOME".to_owned(),
        dir.path().join("home").to_string_lossy().into_owned(),
    )]
    .into_iter()
    .collect();
    assert_eq!(
        maho_x_search::extension::disabled_skills(&project, &env),
        ["x-search"]
    );
}

#[test]
fn a_user_skill_of_the_same_name_wins() {
    let dir = tempfile::tempdir().expect("dir");
    let agent_dir = dir.path().join("agent");
    let user = agent_dir.join("skills").join("x-search").join("SKILL.md");
    std::fs::create_dir_all(user.parent().expect("parent")).expect("mkdir");
    std::fs::write(
        &user,
        "---\nname: x-search\ndescription: My own X search notes.\n---\n\nMine.\n",
    )
    .expect("write");
    let bundled = skill_file(dir.path());
    let loaded = maho_core::skills::load_skills(&maho_core::skills::LoadSkillsOptions {
        cwd: dir.path().to_string_lossy().into_owned(),
        agent_dir: agent_dir.to_string_lossy().into_owned(),
        skill_paths: vec![bundled.to_string_lossy().into_owned()],
        include_defaults: true,
    });
    let found: Vec<_> = loaded
        .skills
        .iter()
        .filter(|skill| skill.name == "x-search")
        .collect();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].description, "My own X search notes.");
}

#[tokio::test]
async fn missing_skill_file_still_registers_the_tool_and_warns_once() {
    let dir = tempfile::tempdir().expect("dir");
    let warnings = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut extension = connected(dir.path());
    extension.skill_file = Some(dir.path().join("missing").join("SKILL.md"));
    extension.warn = {
        let warnings = warnings.clone();
        Arc::new(move |message: &str| warnings.lock().expect("warnings").push(message.to_owned()))
    };
    for _ in 0..2 {
        let mut api = support::api(dir.path());
        extension.register(&mut api);
        assert!(
            api.registered
                .tools
                .iter()
                .any(|tool| tool.definition.name == "x_search")
        );
    }
    assert_eq!(discover(&extension, dir.path()).await, Some(Vec::new()));
    assert_eq!(warnings.lock().expect("warnings").len(), 1);
}

#[tokio::test]
async fn not_connected_contributes_no_skill() {
    let dir = tempfile::tempdir().expect("dir");
    let mut extension = extension(dir.path(), env_of(&[]));
    extension.skill_file = Some(skill_file(dir.path()));
    assert_eq!(discover(&extension, dir.path()).await, None);
}
