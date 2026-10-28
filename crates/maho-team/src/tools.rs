//! The team tools (`team_create`, `team_members`, `team_message`, `team_wait`, `team_stop`) and
//! the `/team` command, contributed to the task extension as a [`TaskAddon`] (spec §2).
use crate::panes::PaneObserver;
use crate::settings::TeamLimits;
use crate::team::{MemberSpec, Team, team_names};
use maho_ext_api::{ToolContent, ToolError, ToolResult};
use maho_task::{AddonCommand, AddonTool, CancelToken, TaskAddon, TaskScope};
use serde_json::{Map, Value, json};
use std::sync::Arc;
use std::time::Duration;

/// Tools this addon registers.
pub const TOOL_NAMES: &[&str] = &[
    "team_create",
    "team_members",
    "team_message",
    "team_wait",
    "team_stop",
];

/// The team addon; `panes` (when given) lets `team_members` report pane ids and open errors.
#[derive(Clone, Default)]
pub struct TeamAddon {
    pub panes: Option<Arc<PaneObserver>>,
}

fn text_result(text: String, details: Value) -> ToolResult {
    ToolResult {
        content: vec![ToolContent::text(text)],
        details: Some(details),
        structured_content: None,
        is_error: None,
    }
}

fn str_param<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn limits(scope: &TaskScope) -> TeamLimits {
    TeamLimits::from_view(scope.view())
}

/// The team named by `team`, or the only team of the session.
fn resolve(scope: &TaskScope, team: Option<&str>) -> Result<Arc<Team>, String> {
    let manager = scope.manager();
    let name = match team {
        Some(name) => name.to_owned(),
        None => {
            let names = team_names(&manager.state().root);
            match names.as_slice() {
                [only] => only.clone(),
                [] => return Err("No team in this session; create one with team_create".into()),
                _ => {
                    return Err(format!(
                        "Several teams exist; pass `team` (one of: {})",
                        names.join(", ")
                    ));
                }
            }
        }
    };
    Team::find(&manager, &name, &limits(scope), scope.notifier())
        .ok_or_else(|| format!("Unknown team \"{name}\""))
}

/// Member items of a `teams.<name>` template (spec §6).
///
/// # Errors
/// Unknown template, or a member without a prompt.
pub fn template_members(
    view: &Value,
    template: &str,
) -> Result<(String, Option<String>, Vec<Value>), String> {
    let entry = view
        .get("teams")
        .and_then(|teams| teams.get(template))
        .ok_or_else(|| {
            format!("Unknown team template \"{template}\" (configure it under `teams`)")
        })?;
    let name = entry
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(template)
        .to_owned();
    let description = entry
        .get("description")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut items = Vec::new();
    for member in entry
        .get("members")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if member.get("isActive").and_then(Value::as_bool) == Some(false) {
            continue;
        }
        let member_name = member
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let prompt = member
            .get("prompt")
            .and_then(Value::as_str)
            .filter(|p| !p.trim().is_empty())
            .ok_or_else(|| format!("Template member \"{member_name}\" has no prompt"))?;
        let mut item = Map::new();
        item.insert("name".into(), json!(member_name));
        item.insert("task".into(), json!(prompt));
        match member.get("kind").and_then(Value::as_str) {
            Some("category") => {
                if let Some(category) = member.get("category") {
                    item.insert("category".into(), category.clone());
                }
            }
            Some("subagent_type") => {
                if let Some(agent) = member.get("subagent_type") {
                    item.insert("agent".into(), agent.clone());
                }
            }
            _ => {}
        }
        if let Some(cwd) = member.get("cwd").and_then(Value::as_str) {
            item.insert("cwd".into(), json!(cwd));
        }
        if member.get("worktreePath").is_some() {
            item.insert("isolated".into(), json!(true));
        }
        if member.get("backendType").and_then(Value::as_str) == Some("tmux") {
            item.insert("pane".into(), json!(true));
        }
        items.push(Value::Object(item));
    }
    Ok((name, description, items))
}

fn member_specs(
    scope: &TaskScope,
    items: &[Value],
    team_panes: Option<bool>,
) -> Result<Vec<MemberSpec>, String> {
    let mut specs = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let name =
            str_param(item, "name").map_or_else(|| format!("member-{}", index + 1), str::to_owned);
        let mut request = scope
            .request(item, None)
            .map_err(|error| format!("member {name}: {error}"))?;
        request.name = None;
        request.pane = item.get("pane").and_then(Value::as_bool).or(team_panes);
        if let Some(cwd) = str_param(item, "cwd") {
            request.cwd = scope.cwd().join(cwd);
        }
        specs.push(MemberSpec { name, request });
    }
    Ok(specs)
}

async fn create(scope: TaskScope, params: Value) -> Result<ToolResult, String> {
    let mut name = str_param(&params, "name").map(str::to_owned);
    let mut context = str_param(&params, "context").map(str::to_owned);
    let mut items: Vec<Value> = params
        .get("members")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(template) = str_param(&params, "template") {
        let (template_name, description, template_items) =
            template_members(scope.view(), template)?;
        name.get_or_insert(template_name);
        if context.is_none() {
            context = description;
        }
        if items.is_empty() {
            items = template_items;
        }
    }
    let name = name.ok_or("Missing required parameter: name")?;
    let specs = member_specs(&scope, &items, params.get("panes").and_then(Value::as_bool))?;
    let team = Team::create(
        scope.manager(),
        &name,
        context,
        specs,
        limits(&scope),
        scope.notifier(),
    )?;
    let file = team.file();
    let mut lines: Vec<String> = file
        .members
        .iter()
        .map(|m| match &m.task_id {
            Some(id) => format!("- {} → task {id} (started)", m.name),
            None => format!("- {} → pending (starts when a slot frees up)", m.name),
        })
        .collect();
    lines.insert(
        0,
        format!(
            "Team {} created with {} members:",
            file.name,
            file.members.len()
        ),
    );
    lines.push(
        "Members run in the background. You get a team update as each one finishes; team_wait collects their results, team_message sends a member a follow-up.".into(),
    );
    let ids: Vec<&str> = file
        .members
        .iter()
        .filter_map(|m| m.task_id.as_deref())
        .collect();
    Ok(text_result(
        lines.join("\n"),
        json!({"team": file.name, "ids": ids}),
    ))
}

fn members(
    scope: &TaskScope,
    params: &Value,
    panes: Option<&PaneObserver>,
) -> Result<ToolResult, String> {
    let tail = usize::try_from(
        params
            .get("tail")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(40),
    )
    .unwrap_or(0);
    let team = str_param(params, "team");
    if team.is_none() {
        let names = team_names(&scope.manager().state().root);
        if names.len() != 1 {
            return Ok(text_result(
                listing(scope, &names, tail, panes),
                json!({"teams": names}),
            ));
        }
    }
    let team = resolve(scope, team)?;
    Ok(text_result(
        team.describe(tail, panes),
        json!({"team": team.name()}),
    ))
}

fn listing(
    scope: &TaskScope,
    names: &[String],
    tail: usize,
    panes: Option<&PaneObserver>,
) -> String {
    if names.is_empty() {
        return "No teams in this session.".into();
    }
    let manager = scope.manager();
    names
        .iter()
        .filter_map(|name| Team::find(&manager, name, &limits(scope), scope.notifier()))
        .map(|team| team.describe(tail, panes))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn tool(
    name: &str,
    label: &str,
    description: &str,
    parameters: Value,
    handler: maho_task::AddonHandler,
) -> AddonTool {
    AddonTool {
        name: name.to_owned(),
        label: label.to_owned(),
        description: description.to_owned(),
        parameters,
        handler,
    }
}

fn member_item_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": {"type": "string", "description": "Member name: lowercase letters, digits, '-'"},
            "task": {"type": "string", "description": "Self-contained assignment for this member"},
            "agent": {"type": "string", "description": "Agent to run as (default `task`)"},
            "category": {"type": "string", "description": "Configured category whose model to use"},
            "model": {"type": "string", "description": "Explicit `provider/model`"},
            "isolated": {"type": "boolean", "description": "Run in an isolated copy of the workspace"},
            "pane": {"type": "boolean", "description": "Follow this member in a terminal pane (when inside tmux/cmux)"}
        },
        "required": ["name", "task"]
    })
}

fn wrap<F, Fut>(f: F) -> maho_task::AddonHandler
where
    F: Fn(TaskScope, Value, CancelToken) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<ToolResult, String>> + Send + 'static,
{
    Arc::new(move |scope, params, cancel| {
        let future = f(scope, params, cancel);
        Box::pin(async move { future.await.map_err(ToolError::Message) })
    })
}

impl TaskAddon for TeamAddon {
    fn tools(&self) -> Vec<AddonTool> {
        let panes = self.panes.clone();
        vec![
            tool(
                "team_create",
                "Team Create",
                "Start a team: several named members, each a background subagent task with its own assignment, sharing an optional `context`. \
Members cannot see this conversation, so every `task` must be self-contained. At most `task.team.max_parallel_members` run at once; the rest start as slots free up. \
You receive a team update when each member finishes; use team_wait to collect results, team_message for follow-ups, team_stop to end the run. \
`template` starts a team configured under `teams`. Inside tmux/cmux members can be followed in panes.",
                json!({"type": "object", "properties": {
                    "name": {"type": "string", "description": "Team name: lowercase letters, digits, '-'"},
                    "context": {"type": "string", "description": "Shared background for every member (goal, constraints, interfaces)"},
                    "members": {"type": "array", "items": member_item_schema()},
                    "template": {"type": "string", "description": "Name of a team configured under `teams`"},
                    "panes": {"type": "boolean", "description": "Show members in terminal panes (default: when inside tmux/cmux)"}
                }}),
                wrap(|scope, params, _| create(scope, params)),
            ),
            tool(
                "team_members",
                "Team Members",
                "List a team's members with their state, task id, model, runs, queued messages and pane; `tail` adds the last lines of each member's output. Without `team`, lists every team of this session.",
                json!({"type": "object", "properties": {
                    "team": {"type": "string"},
                    "tail": {"type": "number", "description": "Trailing output lines per member (0-40)"}
                }}),
                wrap(move |scope, params, _| {
                    let panes = panes.clone();
                    async move { members(&scope, &params, panes.as_deref()) }
                }),
            ),
            tool(
                "team_message",
                "Team Message",
                "Send a member (or `*` for all) a follow-up message. An idle member continues at once in its own session with the message; a busy member gets it as soon as its current run finishes.",
                json!({"type": "object", "properties": {
                    "team": {"type": "string"},
                    "member": {"type": "string", "description": "Member name or `*`"},
                    "message": {"type": "string"}
                }, "required": ["member", "message"]}),
                wrap(|scope, params, _| async move {
                    let team = resolve(&scope, str_param(&params, "team"))?;
                    let member =
                        str_param(&params, "member").ok_or("Missing required parameter: member")?;
                    let lines =
                        team.message(member, str_param(&params, "message").unwrap_or_default())?;
                    Ok(text_result(lines.join("\n"), json!({"team": team.name()})))
                }),
            ),
            tool(
                "team_wait",
                "Team Wait",
                "Wait until the team's members (or the listed `members`) are idle with no queued messages, then return each one's `<task-result>`. `timeout_ms` bounds the wait (task.wait limits).",
                json!({"type": "object", "properties": {
                    "team": {"type": "string"},
                    "members": {"type": "array", "items": {"type": "string"}},
                    "timeout_ms": {"type": "number"}
                }}),
                wrap(|scope, params, cancel| async move {
                    let team = resolve(&scope, str_param(&params, "team"))?;
                    let members: Option<Vec<String>> = params
                        .get("members")
                        .and_then(Value::as_array)
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(Value::as_str)
                                .map(str::to_owned)
                                .collect()
                        });
                    let requested = params
                        .get("timeout_ms")
                        .and_then(Value::as_u64)
                        .map(Duration::from_millis);
                    let timeout = scope.settings().wait_for(requested);
                    let text = team.wait(members, timeout, &cancel).await?;
                    if cancel.is_cancelled() {
                        return Err("aborted".into());
                    }
                    Ok(text_result(text, json!({"team": team.name()})))
                }),
            ),
            tool(
                "team_stop",
                "Team Stop",
                "Stop one member or (without `member`) the whole team: active members are cancelled, pending ones never start, queued messages are dropped.",
                json!({"type": "object", "properties": {
                    "team": {"type": "string"},
                    "member": {"type": "string"}
                }}),
                wrap(|scope, params, _| async move {
                    let team = resolve(&scope, str_param(&params, "team"))?;
                    let lines = team.stop(str_param(&params, "member"));
                    let status = team.file().status;
                    Ok(text_result(
                        format!(
                            "team {} ({}):\n{}",
                            team.name(),
                            status.as_str(),
                            lines.join("\n")
                        ),
                        json!({"team": team.name(), "status": status.as_str()}),
                    ))
                }),
            ),
        ]
    }

    fn commands(&self) -> Vec<AddonCommand> {
        let panes = self.panes.clone();
        vec![AddonCommand {
            name: "team".into(),
            description: "Show the teams of this session and their members".into(),
            handler: Arc::new(move |scope, _raw| {
                let names = team_names(&scope.manager().state().root);
                listing(&scope, &names, 0, panes.as_deref())
            }),
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_mapping() {
        let view = json!({"teams": {"review": {"name": "review", "description": "Review the PR", "members": [
            {"name": "sec", "kind": "category", "category": "deep", "prompt": "check security", "backendType": "tmux", "worktreePath": "/x"},
            {"name": "docs", "kind": "subagent_type", "subagent_type": "scout", "prompt": "check docs", "cwd": "docs"},
            {"name": "off", "kind": "category", "category": "quick", "prompt": "x", "isActive": false}
        ]}}});
        let (name, description, items) = template_members(&view, "review").expect("template");
        assert_eq!(name, "review");
        assert_eq!(description.as_deref(), Some("Review the PR"));
        assert_eq!(items.len(), 2);
        assert_eq!(
            items[0],
            json!({"name": "sec", "task": "check security", "category": "deep", "isolated": true, "pane": true})
        );
        assert_eq!(
            items[1],
            json!({"name": "docs", "task": "check docs", "agent": "scout", "cwd": "docs"})
        );
        assert!(template_members(&view, "nope").is_err());
        let bad = json!({"teams": {"t": {"members": [{"name": "a", "kind": "subagent_type", "subagent_type": "task"}]}}});
        assert!(
            template_members(&bad, "t")
                .unwrap_err()
                .contains("no prompt")
        );
    }
}
