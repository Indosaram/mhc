//! `task_send` to team members (docs/cleanroom/specs/task-control.md §6, acceptance test 7).
use maho_lsp::hooks::HookChain;
use maho_task::runner::BoxFuture;
use maho_task::{
    CancelToken, ModelChoice, RunOutcome, RunSpec, SpawnRequest, TaskAddon, TaskExtension,
    TaskRunner, TaskScope, bundled_agents,
};
use maho_team::{MemberSpec, Team, TeamAddon, TeamLimits};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;

/// Holds every run until it is cancelled.
struct Holding;

impl TaskRunner for Holding {
    fn run<'a>(
        &'a self,
        _spec: &'a RunSpec,
        cancel: CancelToken,
        on_start: Box<dyn FnOnce(Option<u32>) + Send + 'a>,
    ) -> BoxFuture<'a, RunOutcome> {
        Box::pin(async move {
            on_start(None);
            cancel.cancelled().await;
            RunOutcome {
                cancelled: true,
                ..RunOutcome::default()
            }
        })
    }
}

fn member(dir: &Path, name: &str) -> MemberSpec {
    MemberSpec {
        name: name.to_owned(),
        request: SpawnRequest {
            agent: bundled_agents().remove(0),
            task: format!("work as {name}"),
            context: None,
            name: None,
            choice: ModelChoice::default(),
            cwd: dir.to_path_buf(),
            background: true,
            isolated: false,
            group: None,
            pane: None,
            quiet: false,
            owner_session: None,
            root_session: None,
        },
    }
}

fn text_of(result: &maho_ext_api::ToolResult) -> String {
    result
        .content
        .iter()
        .map(|content| match content {
            maho_ext_api::ToolContent::Text { text, .. } => text.clone(),
            _ => String::new(),
        })
        .collect()
}

async fn team_message(scope: &TaskScope, params: Value) -> maho_ext_api::ToolResult {
    let tool = TeamAddon::default()
        .tools()
        .into_iter()
        .find(|tool| tool.name == "team_message")
        .expect("team_message");
    (tool.handler)(scope.clone(), params, CancelToken::default())
        .await
        .expect("team_message result")
}

/// Acceptance test 7: `<team>/<member>` and `<team>/*` give what `team_message` gives.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn team_targets_match_team_message() {
    let dir = tempfile::tempdir().expect("tmp");
    let extension = TaskExtension {
        agent_dir: Some(dir.path().join("agent")),
        runner: Arc::new(Holding),
        available_models: Arc::new(Vec::<String>::new),
        isolation: None,
        observers: Vec::new(),
        hooks: HookChain::default(),
        addons: vec![Arc::new(TeamAddon::default()) as Arc<dyn TaskAddon>],
    };
    let scope = TaskScope::detached(extension, dir.path().to_path_buf(), "lead-session", json!({}));
    let mut teams = Vec::new();
    for name in ["via-tool", "via-send"] {
        let members = vec![member(dir.path(), "a"), member(dir.path(), "b")];
        teams.push(
            Team::create(scope.manager(), name, None, members, TeamLimits::default(), None)
                .expect("team"),
        );
    }

    let expected = team_message(
        &scope,
        json!({"team": "via-tool", "member": "a", "message": "check the tests"}),
    )
    .await;
    let sent = scope
        .send(json!({"to": "via-send/a", "message": "check the tests"}))
        .await
        .expect("task_send");
    assert_eq!(text_of(&sent), text_of(&expected));
    assert!(text_of(&sent).starts_with("a: queued"), "{}", text_of(&sent));
    assert_eq!(sent.details, Some(json!({"team": "via-send"})));
    assert_eq!(expected.details, Some(json!({"team": "via-tool"})));

    let expected = team_message(
        &scope,
        json!({"team": "via-tool", "member": "*", "message": "status?"}),
    )
    .await;
    let sent = scope
        .send(json!({"to": "via-send/*", "message": "status?"}))
        .await
        .expect("task_send");
    assert_eq!(text_of(&sent), text_of(&expected));
    assert_eq!(text_of(&sent).lines().count(), 2);

    // Errors are the same too.
    let unknown = scope
        .send(json!({"to": "via-send/nobody", "message": "hi"}))
        .await;
    assert!(unknown.is_err_and(|error| error.to_string().contains("Unknown member")));

    for team in &teams {
        team.stop(None);
    }
}

async fn call(scope: &TaskScope, name: &str, params: Value) -> maho_ext_api::ToolResult {
    let tool = TeamAddon::default()
        .tools()
        .into_iter()
        .find(|tool| tool.name == name)
        .expect("tool");
    (tool.handler)(scope.clone(), params, CancelToken::default())
        .await
        .expect("tool result")
}

/// Acceptance test 16 through the tools, plus the `owner` rule of `team_task_update`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tasklist_tools() {
    let dir = tempfile::tempdir().expect("tmp");
    let extension = TaskExtension {
        agent_dir: Some(dir.path().join("agent")),
        runner: Arc::new(Holding),
        available_models: Arc::new(Vec::<String>::new),
        isolation: None,
        observers: Vec::new(),
        hooks: HookChain::default(),
        addons: Vec::new(),
    };
    let scope = TaskScope::detached(extension, dir.path().to_path_buf(), "lead", json!({}));
    let team = Team::create(
        scope.manager(),
        "crew",
        None,
        vec![member(dir.path(), "dev")],
        TeamLimits::default(),
        None,
    )
    .expect("team");

    let created = call(&scope, "team_task_create", json!({"subject": "Write   the\nparser", "description": "d"})).await;
    assert!(text_of(&created).contains("#1") && text_of(&created).contains("Write the parser"));
    let blocked = call(&scope, "team_task_create", json!({"subject": "Test it", "description": "", "blocked_by": [1]})).await;
    assert!(text_of(&blocked).contains("blocked by 1"), "{}", text_of(&blocked));

    let refused = call(&scope, "team_task_update", json!({"id": 2, "status": "claimed", "owner": "dev"})).await;
    assert_eq!(refused.details.as_ref().expect("details")["kind"], json!("blocked_by"));
    let stranger = call(&scope, "team_task_update", json!({"id": 1, "status": "claimed", "owner": "stranger"})).await;
    assert_eq!(stranger.details.as_ref().expect("details")["kind"], json!("invalid_arguments"));
    let claimed = call(&scope, "team_task_update", json!({"id": 1, "status": "in_progress", "owner": "dev"})).await;
    assert_eq!(claimed.details.as_ref().expect("details")["item"]["owner"], json!("dev"));
    let cross = call(&scope, "team_task_update", json!({"id": 1, "status": "completed"})).await;
    assert_eq!(cross.details.as_ref().expect("details")["kind"], json!("cross_owner"));

    let tasks_dir = scope.manager().state().root.join("teams").join("crew.tasks");
    std::fs::write(tasks_dir.join("7.json"), "garbage").expect("corrupt");
    let listed = call(&scope, "team_task_list", json!({})).await;
    assert!(text_of(&listed).starts_with("2 items\n#1 [in_progress]"), "{}", text_of(&listed));
    let mine = call(&scope, "team_task_list", json!({"owner": "dev", "status": "in_progress"})).await;
    assert!(text_of(&mine).starts_with("1 item\n#1"), "{}", text_of(&mine));
    let none = call(&scope, "team_task_list", json!({"status": "completed"})).await;
    assert_eq!(text_of(&none), "0 items");

    let missing = call(&scope, "team_task_get", json!({"id": 99})).await;
    assert_eq!(missing.details.as_ref().expect("details")["kind"], json!("not_found"));
    assert_eq!(missing.is_error, None, "a result, not a tool error");
    let found = call(&scope, "team_task_get", json!({"team": "crew", "id": 2})).await;
    assert!(text_of(&found).contains("status: pending") && text_of(&found).contains("blocked_by: 1"));

    team.stop(None);
}
