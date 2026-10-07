mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use maho_ext_api::{
    EventBus, EventKind, EventResult, Extension, ExtensionApi, ExtensionContext, ExtensionEvent,
    ExtensionRuntime, ExtensionSessionProfile, JsonValue, LoadedExtension, SourceInfo, ToolContent,
    ToolResultEvent,
};
use maho_omo_git_master::{
    GitMasterAttributionComponent, GitMasterAttributionComponentOptions, GitMasterCommitFooter,
    GitMasterSettings,
};

const SKILL_PATH: &str = "/home/user/.omo/agent/skills/git-master/SKILL.md";
const BUILTIN_FOOTER: &str = "Ultraworked with [omo](https://github.com/code-yeongyu/oh-my-openagent)";
const CO_AUTHOR_TRAILER: &str = "co-authored-by:";

fn settings(commit_footer: GitMasterCommitFooter) -> GitMasterSettings {
    GitMasterSettings { commit_footer, include_co_authored_by: false }
}

fn registered(component: GitMasterAttributionComponent) -> ExtensionApi {
    let mut api = ExtensionApi::new(
        LoadedExtension::new("git-master", "/tmp".into(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        ExtensionRuntime::default(),
    );
    component.register(&mut api);
    api
}

fn with_settings(settings: GitMasterSettings) -> ExtensionApi {
    registered(GitMasterAttributionComponent::with_load_settings(Arc::new(move |_| settings.clone())))
}

fn config_consumer_api(env: BTreeMap<String, String>) -> ExtensionApi {
    registered(GitMasterAttributionComponent::new(GitMasterAttributionComponentOptions {
        load_settings: None,
        env: Some(env),
    }))
}

fn read_input(input: JsonValue, is_error: bool) -> ExtensionEvent {
    ExtensionEvent::ToolResult(ToolResultEvent {
        tool_call_id: "tc-read-1".into(),
        tool_name: "read".into(),
        input,
        content: vec![ToolContent::text("# Git Master\n\nMode Gate...")],
        details: None,
        is_error,
        usage: None,
    })
}

fn file_path_input(file_path: &str) -> JsonValue {
    JsonValue::Object([("file_path".to_owned(), JsonValue::String(file_path.to_owned()))].into_iter().collect())
}

fn read_result(file_path: &str, is_error: bool) -> ExtensionEvent {
    read_input(file_path_input(file_path), is_error)
}

async fn dispatch_with(api: &ExtensionApi, event: &mut ExtensionEvent, ctx: &ExtensionContext) -> EventResult {
    api.registered.handlers[&EventKind::ToolResult][0](event, ctx).await.expect("test dispatch")
}

async fn dispatch(api: &ExtensionApi, event: &mut ExtensionEvent) -> EventResult {
    dispatch_with(api, event, &support::context()).await
}

fn appended_text(result: &EventResult) -> String {
    let EventResult::ToolResult(result) = result else {
        panic!("expected a tool_result transform");
    };
    let content = result.content.as_ref().expect("appended content");
    match content.last() {
        Some(ToolContent::Text { text, .. }) => text.clone(),
        other => panic!("expected a text content block, got {other:?}"),
    }
}

fn project_config(root: &Path, body: &str) -> (PathBuf, BTreeMap<String, String>) {
    let home = root.join("home");
    let project = home.join("project");
    std::fs::create_dir_all(project.join(".omo")).expect("project config dir");
    std::fs::write(project.join(".omo/omo.jsonc"), body).expect("project config");
    let env = BTreeMap::from([("HOME".to_owned(), home.to_string_lossy().into_owned())]);
    (project, env)
}

#[tokio::test]
async fn default_settings_leave_the_result_untouched() {
    let api = with_settings(settings(GitMasterCommitFooter::Disabled));
    let mut event = read_result(SKILL_PATH, false);
    assert!(matches!(dispatch(&api, &mut event).await, EventResult::None));
}

#[tokio::test]
async fn builtin_footer_appends_the_directive_after_the_read_content() {
    let api = with_settings(settings(GitMasterCommitFooter::Builtin));
    let mut event = read_result(SKILL_PATH, false);
    let result = dispatch(&api, &mut event).await;
    let EventResult::ToolResult(transformed) = &result else {
        panic!("expected a tool_result transform");
    };
    assert_eq!(transformed.content.as_ref().expect("content").len(), 2);
    let appended = appended_text(&result);
    assert!(appended.contains(BUILTIN_FOOTER));
    assert!(!appended.to_lowercase().contains(CO_AUTHOR_TRAILER));
    assert!(!appended.contains("sisyphus-dev-ai"));
    assert!(!appended.contains("users.noreply.github.com"));
}

#[tokio::test]
async fn disabled_footer_leaves_the_result_untouched() {
    let api = with_settings(GitMasterSettings { commit_footer: GitMasterCommitFooter::Disabled, include_co_authored_by: false });
    let mut event = read_result(SKILL_PATH, false);
    assert!(matches!(dispatch(&api, &mut event).await, EventResult::None));
}

#[tokio::test]
async fn a_read_of_an_unrelated_file_is_untouched() {
    let api = with_settings(settings(GitMasterCommitFooter::Builtin));
    let mut event = read_result("/home/user/project/src/index.ts", false);
    assert!(matches!(dispatch(&api, &mut event).await, EventResult::None));
}

#[tokio::test]
async fn a_failed_read_of_the_skill_is_untouched() {
    let api = with_settings(settings(GitMasterCommitFooter::Builtin));
    let mut event = read_result(SKILL_PATH, true);
    assert!(matches!(dispatch(&api, &mut event).await, EventResult::None));
}

#[tokio::test]
async fn a_custom_footer_replaces_the_builtin_text() {
    let api = with_settings(settings(GitMasterCommitFooter::Custom("Shipped with omo".into())));
    let mut event = read_result(SKILL_PATH, false);
    let appended = appended_text(&dispatch(&api, &mut event).await);
    assert!(appended.contains("Shipped with omo"));
    assert!(!appended.contains("Ultraworked with"));
    assert!(!appended.to_lowercase().contains(CO_AUTHOR_TRAILER));
}

#[tokio::test]
async fn the_deprecated_co_author_flag_without_the_footer_is_untouched() {
    let api = with_settings(GitMasterSettings { commit_footer: GitMasterCommitFooter::Disabled, include_co_authored_by: true });
    let mut event = read_result(SKILL_PATH, false);
    assert!(matches!(dispatch(&api, &mut event).await, EventResult::None));
}

#[tokio::test]
async fn a_windows_style_skill_path_is_attributed() {
    let api = with_settings(settings(GitMasterCommitFooter::Builtin));
    let mut event = read_result("C:\\Users\\dev\\.omo\\agent\\skills\\git-master\\SKILL.md", false);
    let appended = appended_text(&dispatch(&api, &mut event).await);
    assert!(appended.contains(BUILTIN_FOOTER));
}

#[tokio::test]
async fn the_path_key_is_accepted_instead_of_file_path() {
    let api = with_settings(settings(GitMasterCommitFooter::Builtin));
    let input = JsonValue::Object([("path".to_owned(), JsonValue::String(SKILL_PATH.to_owned()))].into_iter().collect());
    let mut event = read_input(input, false);
    let appended = appended_text(&dispatch(&api, &mut event).await);
    assert!(appended.contains(BUILTIN_FOOTER));
}

#[tokio::test]
async fn a_non_read_tool_is_untouched() {
    let api = with_settings(settings(GitMasterCommitFooter::Builtin));
    let mut event = read_result(SKILL_PATH, false);
    if let ExtensionEvent::ToolResult(result) = &mut event {
        result.tool_name = "write".into();
    }
    assert!(matches!(dispatch(&api, &mut event).await, EventResult::None));
}

#[tokio::test]
async fn a_non_tool_result_event_is_untouched() {
    let api = with_settings(settings(GitMasterCommitFooter::Builtin));
    let mut event = ExtensionEvent::AgentStart;
    assert!(matches!(dispatch(&api, &mut event).await, EventResult::None));
}

#[tokio::test]
async fn the_settings_loader_receives_the_session_cwd() {
    let recorded: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
    let recorder = Arc::clone(&recorded);
    let api = registered(GitMasterAttributionComponent::with_load_settings(Arc::new(move |cwd: &Path| {
        *recorder.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(cwd.to_path_buf());
        settings(GitMasterCommitFooter::Builtin)
    })));
    let ctx = support::context_with_cwd("/tmp/session-project");
    let mut event = read_result(SKILL_PATH, false);
    let _ = dispatch_with(&api, &mut event, &ctx).await;
    let recorded = recorded.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    assert_eq!(recorded.as_deref(), Some(Path::new("/tmp/session-project")));
}

#[tokio::test]
async fn the_real_config_consumer_appends_the_project_configured_footer() {
    let root = tempfile::tempdir().expect("temp dir");
    let (project, env) = project_config(root.path(), "{\"git_master\":{\"commit_footer\":\"Shipped with omo\"}}");
    let api = config_consumer_api(env);
    let cwd = project.to_string_lossy().into_owned();
    let ctx = support::context_with_cwd(&cwd);
    let mut event = read_result(SKILL_PATH, false);
    let appended = appended_text(&dispatch_with(&api, &mut event, &ctx).await);
    assert!(appended.contains("Shipped with omo"), "the project git_master.commit_footer reaches the directive: {appended}");
    assert!(!appended.contains("Ultraworked with"));
    assert!(!appended.to_lowercase().contains(CO_AUTHOR_TRAILER));
}

#[tokio::test]
async fn the_real_config_consumer_defaults_to_no_footer() {
    let root = tempfile::tempdir().expect("temp dir");
    let (project, env) = project_config(root.path(), "{}");
    let api = config_consumer_api(env);
    let cwd = project.to_string_lossy().into_owned();
    let ctx = support::context_with_cwd(&cwd);
    let mut event = read_result(SKILL_PATH, false);
    assert!(matches!(dispatch_with(&api, &mut event, &ctx).await, EventResult::None));
}
