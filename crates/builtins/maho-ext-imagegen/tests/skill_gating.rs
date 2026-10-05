#[path = "support.rs"]
pub mod support;

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
    let mut event = ExtensionEvent::BeforeAgentStart(BeforeAgentStartEvent { prompt: "draw an image".into(), images: None, system_prompt: "base".into(), system_prompt_options: BuildSystemPromptOptions::default() });
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

#[tokio::test]
async fn omits_a_missing_skill_path_without_crashing() {
    let api = api_with(ImageGen { skill_path: std::path::PathBuf::from("/nonexistent/skill/SKILL.md") });
    let ctx = credentialed();

    assert!(discover(&api, &ctx).await.is_empty());
    assert_eq!(before_agent_start(&api, &ctx).await.as_deref(), Some(format!("base\n{IMAGE_GEN_SECTION}").as_str()));
}
