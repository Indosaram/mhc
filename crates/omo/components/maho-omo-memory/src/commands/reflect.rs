//! `/reflect [--recent N | --conversation <ids>] [focus text]`
//! Port of `components/memory/commands/reflect.ts` at pin 77f3067f1.

use std::sync::Arc;

use maho_ext_api::ExtensionApi;
use memory_core::reflection::ReflectionEvent;

use super::args::{ParseCommandArgsOptions, parse_command_args_full};
use super::types::{
    command_context_from, finish, require_identity, respond, CommandContext, CommandResponse,
    MemoryCommandDeps, NotifyLevel,
};

fn parse_conversation_ids(value: Option<&super::args::FlagValue>) -> Option<Vec<String>> {
    let Some(value) = value else {
        return None;
    };
    let raw = value.as_str()?;
    let ids: Vec<String> = raw
        .split(',')
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty())
        .collect();
    if ids.is_empty() {
        None
    } else {
        Some(ids)
    }
}

pub async fn handle_reflect(
    deps: &MemoryCommandDeps,
    ctx: &CommandContext,
    args: &str,
) -> CommandResponse {
    let identity = match require_identity(deps, ctx) {
        Ok(identity) => identity,
        Err(error) => return respond(ctx, error, NotifyLevel::Error),
    };

    let settings = match (deps.settings)() {
        Ok(settings) => settings,
        Err(error) => return respond(ctx, error, NotifyLevel::Error),
    };
    let trigger = match crate::trigger_wiring::resolve_reflection_trigger_config(
        &settings,
        Some(&identity.identity),
    ) {
        Ok(trigger) => trigger,
        Err(error) => return respond(ctx, error, NotifyLevel::Error),
    };
    if !trigger.enabled {
        return respond(
            ctx,
            "reflection is disabled; set reflection.enabled to true in your omo config to enable it",
            NotifyLevel::Error,
        );
    }

    let Some(sink) = &deps.reflect else {
        return respond(
            ctx,
            "reflection is not available in this session; enable memory reflection in the omo config",
            NotifyLevel::Error,
        );
    };

    let parsed = parse_command_args_full(args, ParseCommandArgsOptions::default());
    let recent_raw = parsed.flag("recent");
    let mut recent_n: Option<usize> = None;
    if let Some(raw) = recent_raw {
        let parsed_value = raw
            .as_str()
            .and_then(|value| value.trim().parse::<usize>().ok());
        match parsed_value {
            Some(value) if value > 0 => recent_n = Some(value),
            _ => {
                return respond(
                    ctx,
                    "--recent expects a positive integer, for example /reflect --recent 5",
                    NotifyLevel::Error,
                );
            }
        }
    }

    let conversation_ids = parse_conversation_ids(parsed.flag("conversation"));
    let focus = parsed.positionals.join(" ").trim().to_owned();
    let focus = if focus.is_empty() { None } else { Some(focus.clone()) };

    let session_id = ctx.session_id.clone().unwrap_or_default();
    let event = ReflectionEvent::Manual {
        focus: focus.clone(),
        recent_n,
        conversation_ids: conversation_ids.clone(),
    };
    let receipt = match sink(&session_id, event) {
        Ok(receipt) => receipt,
        Err(error) => return respond(ctx, error, NotifyLevel::Error),
    };

    let mut scope: Vec<String> = Vec::new();
    if let Some(recent_n) = recent_n {
        scope.push(format!("recent {recent_n}"));
    }
    if let Some(ids) = &conversation_ids {
        scope.push(format!("conversations {}", ids.join(", ")));
    }
    if let Some(focus) = &focus {
        scope.push(format!("focus \"{focus}\""));
    }
    let suffix = if scope.is_empty() {
        String::new()
    } else {
        format!(" ({})", scope.join("; "))
    };

    let (status, run_id) = receipt;
    let text = if status == "active" {
        format!("reflection run {run_id} reserved{suffix}; it starts at the next idle boundary")
    } else {
        format!(
            "reflection request queued as {run_id}{suffix}; a run is already active and this request runs next"
        )
    };
    respond(ctx, text, NotifyLevel::Info)
}

pub fn register_reflect_command(api: &mut ExtensionApi, deps: Arc<MemoryCommandDeps>) {
    api.register_command(
        "reflect",
        Some("Request a memory reflection run now, optionally scoped and focused.".to_owned()),
        Some("[--recent N | --conversation <ids>] [focus]".to_owned()),
        Arc::new(move |args, context| {
            let deps = deps.clone();
            let context = command_context_from(context);
            let args = args.to_owned();
            Box::pin(async move { finish(handle_reflect(&deps, &context, &args).await) })
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::*;

    #[tokio::test]
    async fn given_free_text_focus_when_invoked_then_a_manual_request_is_reserved_and_reported_with_its_run_id() {
        let (_root, identity) = temp_identity();
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_reflect(&fake.deps, &context.ctx, "tighten commit rules").await;

        let requests = reflection_requests(&fake);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].focus.as_deref(), Some("tighten commit rules"));
        assert!(requests[0].recent_n.is_none());
        assert!(response.text.contains("reserved"));
        assert!(response.text.contains("run-1"));
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Info));
    }

    #[tokio::test]
    async fn given_recent_n_when_invoked_then_the_journal_filter_carries_the_count() {
        let (_root, identity) = temp_identity();
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        handle_reflect(&fake.deps, &context.ctx, "--recent 5 keep it short").await;

        let requests = reflection_requests(&fake);
        assert_eq!(requests[0].recent_n, Some(5));
        assert_eq!(requests[0].focus.as_deref(), Some("keep it short"));
    }

    #[tokio::test]
    async fn given_conversation_with_ids_when_invoked_then_the_ids_are_split_into_the_request() {
        let (_root, identity) = temp_identity();
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        handle_reflect(&fake.deps, &context.ctx, "--conversation sess-a,sess-b").await;

        let requests = reflection_requests(&fake);
        assert_eq!(
            requests[0].conversation_ids,
            Some(vec!["sess-a".to_owned(), "sess-b".to_owned()])
        );
    }

    #[tokio::test]
    async fn given_an_active_run_when_the_sink_queues_the_request_then_the_pending_disposition_is_reported() {
        let (_root, identity) = temp_identity();
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        set_receipt(&fake, "pending", "run-7");
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_reflect(&fake.deps, &context.ctx, "").await;

        assert!(response.text.contains("queued"));
        assert!(response.text.contains("run-7"));
    }

    #[tokio::test]
    async fn given_a_non_numeric_recent_when_invoked_then_a_usage_error_is_returned_and_no_request_is_made() {
        let (_root, identity) = temp_identity();
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_reflect(&fake.deps, &context.ctx, "--recent soon").await;

        assert!(reflection_requests(&fake).is_empty());
        assert!(response.text.contains("--recent expects a positive integer"));
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Error));
    }

    #[tokio::test]
    async fn given_no_reflection_sink_when_invoked_then_an_actionable_error_is_returned() {
        let (_root, identity) = temp_identity();
        let fake = fake_deps(
            Some(identity),
            FakeDepsOverrides { reflection_sink: Some(false), ..Default::default() },
        );
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_reflect(&fake.deps, &context.ctx, "").await;

        assert!(response.text.contains("reflection is not available"));
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Error));
    }

    #[tokio::test]
    async fn given_an_unbound_session_when_invoked_then_an_actionable_error_is_returned() {
        let fake = fake_deps(None, FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_reflect(&fake.deps, &context.ctx, "").await;

        assert!(reflection_requests(&fake).is_empty());
        assert!(response.text.contains("not bound"));
    }

    #[tokio::test]
    async fn given_reflection_disabled_when_invoked_then_an_error_is_returned_and_no_request_is_submitted() {
        let (_root, identity) = temp_identity();
        let mut settings = memory_settings();
        settings["reflection"]["enabled"] = serde_json::json!(false);
        let fake = fake_deps(
            Some(identity),
            FakeDepsOverrides { settings: Some(settings), ..Default::default() },
        );
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_reflect(&fake.deps, &context.ctx, "").await;

        assert!(reflection_requests(&fake).is_empty());
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Error));
        assert!(response.text.contains("reflection is disabled"));
    }

    #[tokio::test]
    async fn given_reflection_enabled_in_base_but_disabled_by_per_agent_override_when_invoked_then_an_error_is_returned() {
        let (_root, identity) = temp_identity();
        let mut settings = memory_settings();
        settings["agents"] = serde_json::json!({ TEST_IDENTITY: { "reflection": { "enabled": false } } });
        let fake = fake_deps(
            Some(identity),
            FakeDepsOverrides { settings: Some(settings), ..Default::default() },
        );
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_reflect(&fake.deps, &context.ctx, "").await;

        assert!(reflection_requests(&fake).is_empty());
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Error));
        assert!(!response.text.is_empty());
    }
}
