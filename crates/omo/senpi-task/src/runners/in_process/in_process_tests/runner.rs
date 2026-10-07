//! `in-process.test.ts`, `in-process-model-runtime.test.ts`, `shared-tool-filter.test.ts`,
//! `runtime-fallback-settings.test.ts`, `in-process-runtime-fallback.test.ts`, `marker-suppression.test.ts`.

use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use serde_json::json;

use super::fake_session::FakeSession;
use super::support::{
    base_spec, capturing_runner, fake_runner, last_options, make_tool, make_tracked_tool,
    session_dir_in, session_header, strings, tmp, tool_names, write_session_file,
};
use crate::category::{CategoryResolutionResult, ResolveCategoryOptions, resolve_category};
use crate::host::HostError;
use crate::host::fake::{model, registry};
use crate::manager::child_handle::ManagedChildHandle;
use crate::manager::child_handle::Unsubscribe;
use crate::runners::in_process::ChildCompletionPolicy;
use crate::runners::in_process::child_handle::{
    ChildSession, ChildSessionListener, RunnerFailureKind, RunnerOutcome,
};
use crate::runners::in_process::child_options::HostHandle;
use crate::runners::in_process::runner::{
    ChildPromptEnvelope, ChildSpec, InProcessRunner, InProcessRunnerOptions,
};
use crate::runners::in_process::runtime_fallback_settings::{
    ChildRetryOverride, RetryFallbackSettings, create_runtime_fallback_settings,
};
use crate::runners::in_process::shared_tool_filter::{
    filter_shared_parent_tools, is_task_or_team_family_tool, merge_child_custom_tools,
};
use crate::runners::in_process::subagent_prompt::{SubagentPromptInput, build_subagent_prompt};
use crate::state::{ResolvedModelRecord, ResolvedModelSource};

fn started(
    fake: &Arc<FakeSession>,
    spec: &ChildSpec,
) -> Arc<crate::runners::in_process::child_handle::InProcessChildHandle> {
    let (runner, _captured) = fake_runner(fake);
    let handle = runner.start(spec).expect("start");
    fake.wait_prompt_calls(1);
    handle
}

#[test]
fn given_a_running_child_when_steered_while_the_prompt_is_in_flight_then_the_fake_session_receives_it()
 {
    let dir = tmp();
    let fake = FakeSession::new("child-session-1");
    let handle = started(&fake, &base_spec(&dir.path().to_string_lossy()));
    handle.steer("adjust course").expect("steer");
    assert_eq!(fake.prompt_calls(), 1);
    assert_eq!(fake.steer_calls(), strings(&["adjust course"]));
    fake.set_last_text("done");
    fake.resolve_prompt();
    assert_eq!(handle.wait_for_idle(), RunnerOutcome::completed("done"));
}

#[test]
fn given_a_running_child_when_aborted_mid_run_then_outcome_is_cancelled_and_the_session_was_aborted()
 {
    let dir = tmp();
    let fake = FakeSession::new("child-session-1");
    let handle = started(&fake, &base_spec(&dir.path().to_string_lossy()));
    handle.abort_turn().expect("abort");
    fake.resolve_prompt();
    let outcome = handle.wait_for_idle();
    assert_eq!(fake.abort_calls(), 1);
    assert_eq!(outcome, RunnerOutcome::Cancelled);
}

#[test]
fn given_an_aborted_child_when_a_follow_up_revives_it_then_the_revived_turn_completes_with_new_final_text()
 {
    let dir = tmp();
    let fake = FakeSession::new("child-session-1");
    let handle = started(&fake, &base_spec(&dir.path().to_string_lossy()));
    handle.abort_turn().expect("abort");
    fake.resolve_prompt();
    handle.wait_for_idle();
    handle
        .follow_up_turn("revive with new work")
        .expect("follow up");
    fake.set_last_text("revived final");
    fake.wait_prompt_calls(2);
    fake.resolve_prompt();
    assert_eq!(
        handle.wait_for_idle(),
        RunnerOutcome::completed("revived final")
    );
}

#[test]
fn given_a_completing_child_when_idle_then_the_last_assistant_text_is_extracted() {
    let dir = tmp();
    let fake = FakeSession::new("child-session-1");
    let handle = started(&fake, &base_spec(&dir.path().to_string_lossy()));
    fake.set_last_text("final answer");
    fake.resolve_prompt();
    assert_eq!(
        handle.wait_for_idle(),
        RunnerOutcome::completed("final answer")
    );
    assert_eq!(
        handle.last_assistant_text().as_deref(),
        Some("final answer")
    );
}

#[test]
fn given_shared_and_member_scoped_tools_when_a_child_is_started_then_only_member_scoped_tools_cross_the_family_exclusion()
 {
    let dir = tmp();
    let fake = FakeSession::new("child-session-1");
    let session = Arc::clone(&fake);
    let (runner, captured) = capturing_runner(
        vec![make_tool("grep"), make_tool("task_create")],
        &["render_widget"],
        None,
        move || session.clone(),
    );
    let spec = ChildSpec {
        member_scoped_tools: Some(vec![make_tool("task_send")]),
        ..base_spec(&dir.path().to_string_lossy())
    };
    let handle = runner.start(&spec).expect("start");
    fake.resolve_prompt();
    handle.wait_for_idle();
    let options = last_options(&captured);
    assert_eq!(tool_names(&options), strings(&["grep", "task_send"]));
    for tool in &options.custom_tools {
        assert!(tool.execute("call-1", &json!({})).is_ok());
    }
}

#[test]
fn given_a_tool_allowlist_and_shared_lsp_tools_when_a_child_is_started_then_options_tools_equals_the_allowlist_while_custom_tools_still_carries_the_lsp_tool()
 {
    let dir = tmp();
    let fake = FakeSession::new("child-session-1");
    let session = Arc::clone(&fake);
    let (runner, captured) = capturing_runner(
        vec![make_tool("lsp_diagnostics"), make_tool("grep")],
        &[],
        None,
        move || session.clone(),
    );
    let spec = ChildSpec {
        tool_allowlist: Some(strings(&["read", "find", "grep", "ls", "bash"])),
        ..base_spec(&dir.path().to_string_lossy())
    };
    let handle = runner.start(&spec).expect("start");
    fake.resolve_prompt();
    handle.wait_for_idle();
    let options = last_options(&captured);
    assert_eq!(
        options.tools,
        Some(strings(&["read", "find", "grep", "ls", "bash"]))
    );
    assert_eq!(tool_names(&options), strings(&["lsp_diagnostics", "grep"]));
}

fn bash_tools_for(agent_type: &str) -> Vec<String> {
    let dir = tmp();
    let fake = FakeSession::new("child-session-1");
    let (runner, captured) = fake_runner(&fake);
    let spec = ChildSpec {
        agent_type: Some(agent_type.to_string()),
        tool_allowlist: Some(strings(&["bash"])),
        ..base_spec(&dir.path().to_string_lossy())
    };
    let handle = runner.start(&spec).expect("start");
    fake.resolve_prompt();
    handle.wait_for_idle();
    last_options(&captured)
        .custom_tools
        .iter()
        .filter(|tool| tool.name() == "bash")
        .map(|tool| tool.description().to_string())
        .collect()
}

#[test]
fn given_a_curated_child_with_bash_allowed_when_the_session_is_constructed_then_a_restricted_bash_override_replaces_the_builtin()
 {
    let bash = bash_tools_for("explore");
    assert_eq!(bash.len(), 1);
    assert!(bash[0].contains("read-only"));
}

#[test]
fn given_a_non_curated_child_when_the_session_is_constructed_then_no_bash_override_is_injected() {
    assert!(bash_tools_for("scout").is_empty());
}

#[test]
fn given_a_started_child_when_the_session_is_constructed_then_a_persisted_session_manager_rooted_at_the_spec_session_dir_is_used()
 {
    let dir = tmp();
    let fake = FakeSession::new("child-session-1");
    let (runner, captured) = fake_runner(&fake);
    let spec = base_spec(&dir.path().to_string_lossy());
    let handle = runner.start(&spec).expect("start");
    fake.resolve_prompt();
    handle.wait_for_idle();
    let options = last_options(&captured);
    assert!(options.session_manager.is_persisted());
    assert_eq!(
        Some(options.session_manager.session_dir()),
        spec.session_dir.as_deref()
    );
    assert_eq!(options.resource_loader.extension_count(), 0);
}

#[test]
fn given_a_completed_child_when_the_runner_finishes_then_it_never_disposes_and_dispose_stays_idempotent()
 {
    let dir = tmp();
    let fake = FakeSession::new("child-session-1");
    let handle = started(&fake, &base_spec(&dir.path().to_string_lossy()));
    fake.resolve_prompt();
    handle.wait_for_idle();
    assert_eq!(fake.disposed(), 0);
    handle.dispose_handle();
    handle.dispose_handle();
    assert_eq!(fake.disposed(), 1);
}

#[test]
fn given_a_depth_over_policy_when_start_is_called_then_it_refuses_to_construct_the_session() {
    let dir = tmp();
    let (runner, captured) = capturing_runner(Vec::new(), &[], Some(2), || {
        FakeSession::new("child-session-1")
    });
    let spec = ChildSpec {
        depth: 3,
        ..base_spec(&dir.path().to_string_lossy())
    };
    let error = runner.start(&spec).err().expect("must refuse");
    assert_eq!(error.kind(), RunnerFailureKind::DepthExceeded);
    assert!(captured.lock().expect("captured").is_empty());
}

#[test]
fn given_a_session_that_fails_to_construct_when_start_is_called_then_a_typed_session_create_failure_is_thrown_with_cause()
 {
    let dir = tmp();
    let cause = HostError {
        message: "boot failed".to_string(),
    };
    let thrown = cause.clone();
    let runner = InProcessRunner::new(InProcessRunnerOptions {
        shared_parent_tools: Vec::new(),
        ui_only_tool_names: Vec::new(),
        max_depth: None,
        create_session: Arc::new(move |_options| Err(thrown.clone())),
    });
    let error = runner
        .start(&base_spec(&dir.path().to_string_lossy()))
        .err()
        .expect("must fail");
    assert_eq!(error.kind(), RunnerFailureKind::SessionCreateFailed);
    assert_eq!(error.cause, Some(cause));
}

#[test]
fn given_a_prompt_that_throws_when_the_child_runs_then_a_typed_failure_is_recorded_the_child_stays_resident_and_no_rejection_escapes()
 {
    let dir = tmp();
    let fake = FakeSession::new("child-session-1");
    let handle = started(&fake, &base_spec(&dir.path().to_string_lossy()));
    fake.reject_prompt("prompt boom");
    assert_eq!(
        handle.wait_for_idle(),
        RunnerOutcome::error(RunnerFailureKind::ChildPromptFailed, "prompt boom")
    );
    assert_eq!(fake.disposed(), 0);
}

#[test]
fn given_a_subscribed_listener_when_the_child_emits_lifecycle_events_then_it_observes_agent_start_before_agent_end()
 {
    let dir = tmp();
    let fake = FakeSession::new("child-session-1");
    let handle = started(&fake, &base_spec(&dir.path().to_string_lossy()));
    let seen: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    let sink = Arc::clone(&seen);
    let _unsubscribe = ManagedChildHandle::subscribe(
        handle.as_ref(),
        Arc::new(move |event| sink.lock().expect("seen").push(event.event_type.clone())),
    );
    fake.emit(&json!({ "type": "agent_start" }));
    fake.emit(&json!({ "type": "agent_end" }));
    fake.resolve_prompt();
    handle.wait_for_idle();
    assert_eq!(
        *seen.lock().expect("seen"),
        strings(&["agent_start", "agent_end"])
    );
}

fn thinking_level_for(level: Option<&str>) -> Option<String> {
    let dir = tmp();
    let (runner, captured) = capturing_runner(Vec::new(), &[], None, || {
        FakeSession::immediate("child-session-1", None)
    });
    let spec = ChildSpec {
        thinking_level: level.map(str::to_string),
        ..base_spec(&dir.path().to_string_lossy())
    };
    runner.start(&spec).expect("start").wait_for_idle();
    last_options(&captured).thinking_level
}

#[test]
fn given_a_spec_carrying_a_thinking_level_when_the_child_session_is_created_then_the_level_reaches_the_senpi_session_options()
 {
    assert_eq!(thinking_level_for(Some("xhigh")).as_deref(), Some("xhigh"));
}

#[test]
fn given_a_spec_without_a_thinking_level_when_the_child_session_is_created_then_no_level_is_forced_so_senpi_keeps_its_default()
 {
    assert_eq!(thinking_level_for(None), None);
}

#[test]
fn given_a_parent_model_runtime_when_the_child_session_is_constructed_then_native_providers_remain_executable()
 {
    let dir = tmp();
    let model_runtime: HostHandle = Arc::new("native-provider-runtime");
    let (runner, captured) = capturing_runner(Vec::new(), &[], None, || {
        FakeSession::immediate("child-session", None)
    });
    let spec = ChildSpec {
        model_runtime: Some(Arc::clone(&model_runtime)),
        ..base_spec(&dir.path().to_string_lossy())
    };
    runner.start(&spec).expect("start").wait_for_idle();
    let passed = last_options(&captured)
        .model_runtime
        .expect("model runtime");
    assert!(Arc::ptr_eq(&passed, &model_runtime));
}

#[test]
fn given_task_team_and_dag_orchestration_names_when_classified_then_only_orchestration_names_match()
{
    for name in ["dag", "task", "task_create", "task_send", "team_create"] {
        assert!(is_task_or_team_family_tool(name), "{name}");
    }
    for name in ["grep", "taskmaster"] {
        assert!(!is_task_or_team_family_tool(name), "{name}");
    }
}

#[test]
fn given_shared_tools_with_family_and_ui_only_entries_when_filtered_then_family_and_ui_only_removed()
 {
    let shared = vec![
        make_tool("grep"),
        make_tool("task_create"),
        make_tool("team_create"),
        make_tool("render_widget"),
    ];
    let filtered = filter_shared_parent_tools(&shared, &strings(&["render_widget"]));
    let names: Vec<&str> = filtered.iter().map(|tool| tool.name()).collect();
    assert_eq!(names, ["grep"]);
}

#[test]
fn given_family_tool_in_shared_and_in_member_scoped_when_merged_then_only_member_scoped_family_crosses_the_exclusion()
 {
    let ran = Arc::new(AtomicBool::new(false));
    let shared = vec![make_tool("grep"), make_tool("task")];
    let member_scoped = vec![make_tracked_tool("task_send", &ran)];
    let merged = merge_child_custom_tools(&shared, Some(&member_scoped), &[]);
    let names: Vec<&str> = merged.iter().map(|tool| tool.name()).collect();
    assert_eq!(names, ["grep", "task_send"]);
    merged[1].execute("call-1", &json!({})).expect("execute");
    assert!(ran.load(Ordering::SeqCst));
}

#[test]
fn given_no_member_scoped_tools_when_merged_then_result_is_only_the_filtered_shared_set() {
    let shared = vec![make_tool("glob"), make_tool("task_update")];
    let merged = merge_child_custom_tools(&shared, None, &[]);
    let names: Vec<&str> = merged.iter().map(|tool| tool.name()).collect();
    assert_eq!(names, ["glob"]);
}

fn record(
    provider: &str,
    model_id: &str,
    reasoning_effort: Option<&str>,
    variant: Option<&str>,
) -> ResolvedModelRecord {
    ResolvedModelRecord {
        provider: provider.to_string(),
        model_id: model_id.to_string(),
        display: format!("{provider}/{model_id}"),
        source: ResolvedModelSource::Category,
        variant: variant.map(str::to_string),
        reasoning_effort: reasoning_effort.map(str::to_string),
        reasoning: None,
    }
}

#[test]
fn given_no_child_fallback_chain_when_settings_are_created_then_global_model_fallback_is_disabled()
{
    assert_eq!(
        create_runtime_fallback_settings(Some("vendor/primary"), None, None),
        RetryFallbackSettings::default()
    );
}

#[test]
fn given_an_explicit_child_fallback_chain_when_settings_are_created_then_only_that_chain_is_enabled()
 {
    let settings = create_runtime_fallback_settings(
        Some("vendor/primary"),
        Some(&[record("vendor", "fallback", None, None)]),
        None,
    );
    assert!(settings.model_fallback);
    assert_eq!(settings.chains.len(), 1);
    assert_eq!(
        settings.chains["vendor/primary"],
        strings(&["vendor/fallback"])
    );
}

/// A child-owned retry override; each field is optional and independent (upstream spreads a key
/// only when it is defined, so `None` keeps the engine's own default for that key).
fn child_retry(max_retries: Option<u32>, base_delay_ms: Option<u64>) -> ChildRetryOverride {
    ChildRetryOverride {
        max_retries,
        base_delay_ms,
    }
}

#[test]
fn given_a_child_retry_budget_beside_the_chain_when_settings_are_created_then_the_budget_is_set_and_the_chain_stays_enabled()
{
    let settings = create_runtime_fallback_settings(
        Some("vendor/primary"),
        Some(&[record("vendor", "fallback", None, None)]),
        Some(&child_retry(Some(1), None)),
    );
    assert!(settings.model_fallback);
    assert_eq!(settings.max_retries, Some(1));
    assert_eq!(settings.base_delay_ms, None);
    assert_eq!(
        settings.chains["vendor/primary"],
        strings(&["vendor/fallback"])
    );
}

#[test]
fn given_a_child_retry_budget_without_a_chain_when_settings_are_created_then_the_budget_is_kept_while_model_fallback_stays_disabled()
{
    let settings = create_runtime_fallback_settings(
        Some("vendor/primary"),
        None,
        Some(&child_retry(Some(1), None)),
    );
    assert!(!settings.model_fallback);
    assert!(settings.chains.is_empty());
    assert_eq!(settings.max_retries, Some(1));
    assert_eq!(settings.base_delay_ms, None);
}

#[test]
fn given_only_a_base_delay_override_when_settings_are_created_then_the_delay_is_set_independently_of_max_retries()
{
    let settings = create_runtime_fallback_settings(
        Some("vendor/primary"),
        Some(&[record("vendor", "fallback", None, None)]),
        Some(&child_retry(None, Some(250))),
    );
    assert_eq!(settings.max_retries, None);
    assert_eq!(settings.base_delay_ms, Some(250));
    assert!(settings.model_fallback);
}

#[test]
fn given_a_zero_max_retries_override_when_settings_are_created_then_zero_is_kept_distinct_from_absent()
{
    let settings = create_runtime_fallback_settings(
        Some("vendor/primary"),
        None,
        Some(&child_retry(Some(0), None)),
    );
    assert_eq!(settings.max_retries, Some(0));
    assert_ne!(settings.max_retries, None);
    assert_eq!(settings.base_delay_ms, None);
}

fn retry_settings_for(fallbacks: Vec<ResolvedModelRecord>) -> RetryFallbackSettings {
    let dir = tmp();
    let (runner, captured) = capturing_runner(Vec::new(), &[], None, || {
        FakeSession::immediate("runtime-fallback-child", None)
    });
    let spec = ChildSpec {
        selected_model: Some("kimi-coding/kimi-for-coding-highspeed-unlocked".to_string()),
        fallback_models: Some(fallbacks),
        ..base_spec(&dir.path().to_string_lossy())
    };
    runner.start(&spec).expect("start").wait_for_idle();
    last_options(&captured).settings
}

#[test]
fn given_a_selected_model_and_ordered_fallbacks_when_the_child_session_is_created_then_child_local_runtime_fallback_settings_preserve_the_chain()
 {
    let settings = retry_settings_for(vec![
        record("quotio-openai", "gpt-5.6-luna-fast", Some("minimal"), None),
        record(
            "example-gateway",
            "z-ai/glm-5.2-ultrafast-unlocked",
            Some("none"),
            None,
        ),
    ]);
    assert!(settings.model_fallback);
    assert_eq!(
        settings.chains["kimi-coding/kimi-for-coding-highspeed-unlocked"],
        strings(&[
            "quotio-openai/gpt-5.6-luna-fast:minimal",
            "example-gateway/z-ai/glm-5.2-ultrafast-unlocked:none",
        ])
    );
}

#[test]
fn given_runtime_fallback_models_with_both_reasoning_effort_and_variant_when_the_child_session_is_created_then_reasoning_effort_wins_over_variant()
 {
    let settings = retry_settings_for(vec![record(
        "quotio-openai",
        "gpt-5.6-luna-fast",
        Some("high"),
        Some("low"),
    )]);
    assert_eq!(
        settings.chains["kimi-coding/kimi-for-coding-highspeed-unlocked"],
        strings(&["quotio-openai/gpt-5.6-luna-fast:high"])
    );
}

#[test]
fn given_an_agent_dir_with_a_marker_extension_when_a_child_boots_through_the_runner_then_the_factory_never_runs_and_parent_tools_survive()
 {
    let dir = tmp();
    let ran = Arc::new(AtomicBool::new(false));
    let prompts = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&prompts);
    let (runner, captured) = capturing_runner(
        vec![make_tracked_tool("marker_parent_tool", &ran)],
        &[],
        None,
        move || {
            let counter = Arc::clone(&counter);
            let session: Arc<dyn ChildSession> = FakeSession::immediate(
                "marker-session",
                Some(Box::new(move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                })),
            );
            session
        },
    );
    let spec = ChildSpec {
        task_id: "task-marker".to_string(),
        agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
        prompt: "inspect only".to_string(),
        ..base_spec(&dir.path().join("child-sessions").to_string_lossy())
    };
    let handle = runner.start(&spec).expect("start");
    handle.wait_for_idle();
    let options = last_options(&captured);
    assert_eq!(options.resource_loader.extension_count(), 0);
    assert_eq!(options.agent_dir, spec.agent_dir);
    let parent_tool = options
        .custom_tools
        .iter()
        .find(|tool| tool.name() == "marker_parent_tool")
        .expect("parent tool survives");
    parent_tool.execute("call-1", &json!({})).expect("execute");
    assert!(ran.load(Ordering::SeqCst));
    assert_eq!(ManagedChildHandle::task_id(handle.as_ref()), "task-marker");
    assert_eq!(prompts.load(Ordering::SeqCst), 1);
}

/// Child-local retry settings for an explicit selected model and fallback list, through the runner.
fn retry_settings_for_spec(
    selected_model: Option<&str>,
    fallbacks: Option<Vec<ResolvedModelRecord>>,
) -> RetryFallbackSettings {
    let dir = tmp();
    let (runner, captured) = capturing_runner(Vec::new(), &[], None, || {
        FakeSession::immediate("runtime-fallback-child", None)
    });
    let spec = ChildSpec {
        selected_model: selected_model.map(str::to_string),
        fallback_models: fallbacks,
        ..base_spec(&dir.path().to_string_lossy())
    };
    runner.start(&spec).expect("start").wait_for_idle();
    last_options(&captured).settings
}

#[test]
fn given_a_builtin_category_resolving_to_a_chain_rung_when_the_child_session_is_created_then_the_remaining_chain_rungs_land_in_retry_fallback_chains()
 {
    let models = registry(vec![
        model("quotio-openai", "gpt-5.6-luna-fast"),
        model("opencode-go", "minimax-m3"),
    ]);
    let resolution = resolve_category(
        "quick",
        &json!({}),
        &models,
        &ResolveCategoryOptions::default(),
    )
    .expect("resolve");
    let spec = match resolution {
        CategoryResolutionResult::Resolved { spec, .. } => *spec,
        other => panic!("Expected resolved category, got {}", other.kind()),
    };
    let selected_model = format!("{}/{}", spec.provider, spec.model_id);
    assert_eq!(selected_model, "quotio-openai/gpt-5.6-luna-fast");
    let settings = retry_settings_for_spec(Some(&selected_model), spec.fallback_models.clone());
    assert!(settings.model_fallback);
    assert_eq!(
        settings.chains[&selected_model],
        strings(&["opencode-go/minimax-m3:max"])
    );
}

#[test]
fn given_no_runtime_fallbacks_when_the_child_session_is_created_then_global_model_fallback_is_disabled()
{
    let settings = retry_settings_for_spec(None, None);
    assert_eq!(settings, RetryFallbackSettings::default());
    assert!(!settings.model_fallback);
    assert!(settings.chains.is_empty());
}

#[test]
fn given_no_child_system_prompt_when_the_child_session_is_created_then_the_options_leave_it_absent()
{
    let dir = tmp();
    let (runner, captured) = capturing_runner(Vec::new(), &[], None, || {
        FakeSession::immediate("system-prompt-child", None)
    });
    let spec = ChildSpec {
        system_prompt: None,
        ..base_spec(&dir.path().to_string_lossy())
    };
    runner.start(&spec).expect("start").wait_for_idle();
    assert_eq!(last_options(&captured).system_prompt, None);
}

#[test]
fn given_a_supplied_system_prompt_beside_instructions_when_the_child_session_is_created_then_the_options_carry_the_exact_separate_persona()
{
    let dir = tmp();
    let persona = "# Kibitzer\n\nYou judge turns.";
    let (runner, captured) = capturing_runner(Vec::new(), &[], None, || {
        FakeSession::immediate("system-prompt-child", None)
    });
    let spec = ChildSpec {
        system_prompt: Some(persona.to_string()),
        instructions: Some("work carefully".to_string()),
        ..base_spec(&dir.path().to_string_lossy())
    };
    runner.start(&spec).expect("start").wait_for_idle();
    let options = last_options(&captured);
    assert_eq!(options.system_prompt.as_deref(), Some(persona));
    assert_ne!(options.system_prompt.as_deref(), spec.instructions.as_deref());
}

#[test]
fn given_a_supplied_system_prompt_when_the_child_is_resumed_then_the_options_carry_it_unchanged()
{
    let dir = tmp();
    let persona = "# Kibitzer\n\nYou judge turns.";
    let (runner, captured) = capturing_runner(Vec::new(), &[], None, || {
        FakeSession::immediate("system-prompt-child", None)
    });
    let spec = ChildSpec {
        system_prompt: Some(persona.to_string()),
        ..base_spec(&session_dir_in(&dir, "task-system-prompt"))
    };
    let session_file_dir = tmp();
    let session_path =
        write_session_file(&session_file_dir, &session_header("system-prompt-session"));
    runner.start(&spec).expect("start").wait_for_idle();
    runner.resume(&spec, &session_path).expect("resume");
    let all = captured.lock().expect("captured").clone();
    let resume_options = all.last().expect("resume options");
    assert_eq!(resume_options.system_prompt.as_deref(), Some(persona));
}

/// A session that records every prompt text it receives, so a test can assert the EXACT initial
/// user message the runner delivers through the real `runner.start` path (never a re-derived copy).
#[derive(Default)]
struct RecordingSession {
    session_id: String,
    prompts: Mutex<Vec<String>>,
}

impl RecordingSession {
    fn new(session_id: &str) -> Arc<Self> {
        Arc::new(Self {
            session_id: session_id.to_string(),
            prompts: Mutex::new(Vec::new()),
        })
    }

    fn prompts(&self) -> Vec<String> {
        self.prompts.lock().expect("recorded prompts").clone()
    }
}

impl ChildSession for RecordingSession {
    fn session_id(&self) -> String {
        self.session_id.clone()
    }

    fn prompt(&self, text: &str) -> Result<(), HostError> {
        self.prompts
            .lock()
            .expect("recorded prompts")
            .push(text.to_string());
        Ok(())
    }

    fn steer(&self, _text: &str) -> Result<(), HostError> {
        Ok(())
    }

    fn follow_up(&self, _text: &str) -> Result<(), HostError> {
        Ok(())
    }

    fn abort(&self) -> Result<(), HostError> {
        Ok(())
    }

    fn subscribe(&self, _listener: ChildSessionListener) -> Unsubscribe {
        Box::new(|| {})
    }

    fn get_last_assistant_text(&self) -> Option<String> {
        None
    }

    fn dispose(&self) {}
}

fn recording_runner() -> (InProcessRunner, Arc<RecordingSession>) {
    let recorder = RecordingSession::new("envelope-child");
    let returned: Arc<dyn ChildSession> = recorder.clone();
    let (runner, _captured) =
        capturing_runner(Vec::new(), &[], None, move || Arc::clone(&returned));
    (runner, recorder)
}

fn subagent_renderer(spec: &ChildSpec) -> String {
    build_subagent_prompt(&SubagentPromptInput {
        task_id: &spec.task_id,
        parent_session_id: &spec.parent_session_id,
        root_session_id: &spec.root_session_id,
        depth: spec.depth,
        agent_type: spec.agent_type.as_deref(),
        instructions: spec.instructions.as_deref(),
        prompt: &spec.prompt,
    })
}

#[test]
fn given_an_absent_prompt_envelope_when_the_child_starts_then_the_delivered_prompt_equals_the_subagent_renderer()
{
    let dir = tmp();
    let spec = ChildSpec {
        prompt: "framed payload".to_string(),
        instructions: Some("be careful".to_string()),
        ..base_spec(&dir.path().to_string_lossy())
    };
    let (runner, recorder) = recording_runner();
    runner.start(&spec).expect("start").wait_for_idle();
    assert_eq!(recorder.prompts(), vec![subagent_renderer(&spec)]);
}

#[test]
fn given_an_explicit_subagent_envelope_when_the_child_starts_then_the_delivered_prompt_equals_the_absent_case_and_the_renderer()
{
    let dir = tmp();
    let absent = ChildSpec {
        prompt: "framed payload".to_string(),
        instructions: Some("be careful".to_string()),
        ..base_spec(&dir.path().to_string_lossy())
    };
    let explicit = ChildSpec {
        prompt_envelope: Some(ChildPromptEnvelope::Subagent),
        ..absent.clone()
    };

    let (runner, recorder) = recording_runner();
    runner.start(&absent).expect("start").wait_for_idle();
    let absent_prompt = recorder.prompts();

    let (runner, recorder) = recording_runner();
    runner.start(&explicit).expect("start").wait_for_idle();
    let explicit_prompt = recorder.prompts();

    assert_eq!(absent_prompt, vec![subagent_renderer(&absent)]);
    assert_eq!(explicit_prompt, vec![subagent_renderer(&explicit)]);
    assert_eq!(explicit_prompt, absent_prompt);
}

#[test]
fn given_a_bare_prompt_envelope_when_the_child_starts_then_the_initial_user_message_is_the_prompt_byte_for_byte_without_instructions()
{
    let dir = tmp();
    let sentinel = "\n  <kibitzer-input>\npayload line\n</kibitzer-input>\n  ".to_string();
    let spec = ChildSpec {
        prompt: sentinel.clone(),
        prompt_envelope: Some(ChildPromptEnvelope::Bare),
        instructions: Some("INSTRUCTIONS_SENTINEL_MUST_NOT_APPEAR".to_string()),
        ..base_spec(&dir.path().to_string_lossy())
    };
    let (runner, recorder) = recording_runner();
    runner.start(&spec).expect("start").wait_for_idle();
    let prompts = recorder.prompts();
    assert_eq!(prompts, vec![spec.prompt.clone()]);
    assert!(!prompts[0].contains("INSTRUCTIONS_SENTINEL_MUST_NOT_APPEAR"));
}

#[test]
fn given_a_bare_prompt_envelope_when_the_child_is_resumed_then_no_new_prompt_is_delivered()
{
    let dir = tmp();
    let spec = ChildSpec {
        prompt: "self-contained block".to_string(),
        prompt_envelope: Some(ChildPromptEnvelope::Bare),
        ..base_spec(&session_dir_in(&dir, "task-bare-resume"))
    };
    let session_file_dir = tmp();
    let session_path =
        write_session_file(&session_file_dir, &session_header("bare-resume-session"));
    let (runner, recorder) = recording_runner();
    runner.resume(&spec, &session_path).expect("resume");
    assert!(recorder.prompts().is_empty());
}

fn textless_turn_end() -> serde_json::Value {
    json!({ "type": "message_end", "message": { "role": "assistant", "content": [], "stopReason": "stop" } })
}

#[test]
fn given_a_child_spec_with_the_turn_completion_policy_when_its_textless_turn_settles_then_the_runner_handle_reports_a_completed_empty_turn()
{
    let dir = tmp();
    let fake = FakeSession::new("completion-child");
    let returned: Arc<dyn ChildSession> = fake.clone();
    let (runner, _captured) = capturing_runner(Vec::new(), &[], None, move || Arc::clone(&returned));
    let spec = ChildSpec {
        completion: Some(ChildCompletionPolicy::Turn),
        ..base_spec(&dir.path().to_string_lossy())
    };
    let handle = runner.start(&spec).expect("start");
    fake.wait_prompt_calls(1);
    fake.emit(&textless_turn_end());
    fake.resolve_prompt();
    assert_eq!(handle.wait_for_idle(), RunnerOutcome::completed(""));
}

#[test]
fn given_a_child_spec_without_a_completion_policy_when_its_textless_turn_settles_then_the_runner_handle_reports_a_child_turn_failed_error()
{
    let dir = tmp();
    let fake = FakeSession::new("completion-child");
    let returned: Arc<dyn ChildSession> = fake.clone();
    let (runner, _captured) = capturing_runner(Vec::new(), &[], None, move || Arc::clone(&returned));
    let spec = ChildSpec {
        completion: None,
        ..base_spec(&dir.path().to_string_lossy())
    };
    let handle = runner.start(&spec).expect("start");
    fake.wait_prompt_calls(1);
    fake.emit(&textless_turn_end());
    fake.resolve_prompt();
    match handle.wait_for_idle() {
        RunnerOutcome::Error { failure, .. } => {
            assert_eq!(failure.kind, RunnerFailureKind::ChildTurnFailed);
        }
        other => panic!("expected error, got {other:?}"),
    }
}

#[test]
fn given_a_child_spec_with_the_turn_completion_policy_when_the_child_is_resumed_then_the_restored_handle_drains_a_completed_empty_turn()
{
    let dir = tmp();
    let fake = FakeSession::new("completion-child");
    let returned: Arc<dyn ChildSession> = fake.clone();
    let (runner, _captured) = capturing_runner(Vec::new(), &[], None, move || Arc::clone(&returned));
    let spec = ChildSpec {
        completion: Some(ChildCompletionPolicy::Turn),
        ..base_spec(&session_dir_in(&dir, "task-completion-resume"))
    };
    let session_file_dir = tmp();
    let session_path =
        write_session_file(&session_file_dir, &session_header("completion-resume-session"));
    let handle = runner.resume(&spec, &session_path).expect("resume");
    assert_eq!(fake.prompt_calls(), 0);
    assert_eq!(handle.wait_for_idle(), RunnerOutcome::completed(""));
}
