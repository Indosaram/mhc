use crate::support;

use maho_ext_api::*;
use maho_ext_imagegen::{ImageGen, IMAGE_GEN_SECTION};
use std::sync::Arc;

fn api_with(image_gen: ImageGen) -> ExtensionApi {
    let mut api = ExtensionApi::new(
        LoadedExtension::new("imagegen", "/tmp".into(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        ExtensionRuntime::default(),
    );
    image_gen.register(&mut api);
    api
}

fn credentialed() -> ExtensionContext {
    let root = std::path::Path::new("/tmp");
    support::context(root, support::registry(false))
}

fn uncredentialed() -> ExtensionContext {
    let root = std::path::Path::new("/tmp");
    support::context(root, Arc::new(support::FixtureRegistry { stored_api_key: false, provider_api_key: None, provider_headers: None, models: Vec::new() }))
}

async fn discover(api: &ExtensionApi, ctx: &ExtensionContext) -> Vec<String> {
    let mut event = ExtensionEvent::ResourcesDiscover(ResourcesDiscoverEvent { cwd: ctx.cwd.clone(), reason: SessionReason::Startup, scoped_entries: false });
    match (api.registered.handlers[&EventKind::ResourcesDiscover][0])(&mut event, ctx).await.expect("discover") {
        EventResult::ResourcesDiscover(result) => result.skill_paths.into_iter().map(|entry| entry.path).collect(),
        EventResult::None => Vec::new(),
        _ => panic!("unexpected discover result"),
    }
}

async fn before_agent_start(api: &ExtensionApi, ctx: &ExtensionContext) -> Option<String> {
    let mut event = ExtensionEvent::BeforeAgentStart(BeforeAgentStartEvent { prompt: "draw an image".into(), trigger: "prompt".into(), preview: false, images: None, system_prompt: "base".into(), system_prompt_options: BuildSystemPromptOptions::default() });
    match (api.registered.handlers[&EventKind::BeforeAgentStart][0])(&mut event, ctx).await.expect("before agent start") {
        EventResult::BeforeAgentStart(result) => result.system_prompt,
        EventResult::None => None,
        _ => panic!("unexpected before-agent-start result"),
    }
}

#[tokio::test]
async fn contributes_and_loads_the_skill_when_credentials_exist() {
    let api = api_with(ImageGen::default());
    let ctx = credentialed();

    let skill_paths = discover(&api, &ctx).await;
    assert_eq!(skill_paths.len(), 1);
    assert!(std::path::Path::new(&skill_paths[0]).is_file(), "bundled skill must exist on disk");
    assert_eq!(before_agent_start(&api, &ctx).await.as_deref(), Some(format!("base\n{IMAGE_GEN_SECTION}").as_str()));
}

#[tokio::test]
async fn contributes_no_skill_or_section_when_credentials_are_absent() {
    let api = api_with(ImageGen::default());
    let ctx = uncredentialed();

    assert!(discover(&api, &ctx).await.is_empty());
    assert_eq!(before_agent_start(&api, &ctx).await, None);
}

#[test]
fn embedded_skill_materializes_the_bundled_text_in_a_private_agent_dir_folder() {
    let agent_dir = tempfile::tempdir().expect("agent dir");
    let path = maho_ext_imagegen::embedded_skill_path(agent_dir.path()).expect("embedded skill path");
    let skill_dir = maho_ext_imagegen::embedded_skill_dir(agent_dir.path());
    assert_eq!(path, skill_dir.join("SKILL.md"));
    assert!(skill_dir.starts_with(agent_dir.path().join("cache")));
    let bundled = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/skill/SKILL.md")).expect("bundled skill");
    assert_eq!(std::fs::read_to_string(&path).expect("embedded skill"), bundled);
    #[cfg(unix)]
    for dir in [skill_dir.parent().expect("version dir"), skill_dir.as_path()] {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir).expect("dir metadata").permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "{}", dir.display());
    }
}

#[cfg(unix)]
#[test]
fn embedded_skill_refuses_a_symlinked_skill_folder() {
    let agent_dir = tempfile::tempdir().expect("agent dir");
    let elsewhere = tempfile::tempdir().expect("elsewhere");
    let skill_dir = maho_ext_imagegen::embedded_skill_dir(agent_dir.path());
    std::fs::create_dir_all(skill_dir.parent().expect("version dir")).expect("version dir");
    std::os::unix::fs::symlink(elsewhere.path(), &skill_dir).expect("symlink");
    assert!(maho_ext_imagegen::embedded_skill_path(agent_dir.path()).is_none());
    assert!(!elsewhere.path().join("SKILL.md").exists());
}

#[tokio::test]
async fn omits_a_missing_skill_path_without_crashing() {
    let api = api_with(ImageGen { skill_path: std::path::PathBuf::from("/nonexistent/skill/SKILL.md") });
    let ctx = credentialed();

    assert!(discover(&api, &ctx).await.is_empty());
    assert_eq!(before_agent_start(&api, &ctx).await.as_deref(), Some(format!("base\n{IMAGE_GEN_SECTION}").as_str()));
}
