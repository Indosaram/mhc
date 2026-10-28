//! Acceptance test 14 of `docs/cleanroom/specs/question-gates.md` §5: end to end through a real
//! session with the ask-user builtin. Time is paused; the question's own 30-minute timer fires
//! when the runtime goes idle, so no wall-clock wait is involved.

use maho_ai::providers::faux::{RegisterFauxProviderOptions, faux_provider, faux_streams};
use maho_core::agent_session::{
    AgentSession, AgentSessionConfig, ExecuteToolOptions, ExtensionBindings,
};
use maho_ext_api::*;
use maho_ext_ask_user::{
    AskUser,
    format::{format_required_no_answer, format_result_text},
    schema::{AskUserVariant, to_canonical},
};
use maho_ext_host::{
    ExtensionRunner,
    loader::{NativeExtensionFactory, load_extensions},
};
use serde_json::{Value, json};
use std::sync::Arc;

type TestError = Box<dyn std::error::Error + Send + Sync>;

/// An attached UI whose user never answers: the question stays open until its signal fires.
struct SilentUi;
impl ExtensionUi for SilentUi {
    fn select<'a>(
        &'a self,
        _: &'a str,
        _: &'a [String],
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn confirm<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, bool> {
        Box::pin(async { false })
    }
    fn input<'a>(
        &'a self,
        _: &'a str,
        _: Option<&'a str>,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn question(
        &self,
        request: QuestionRequest,
        options: QuestionOptions,
    ) -> ExtensionFuture<'_, QuestionResponse> {
        Box::pin(async move {
            match &options.dialog.signal {
                Some(signal) => signal.cancelled().await,
                None => std::future::pending::<()>().await,
            }
            Ok(QuestionResponse {
                status: QuestionStatus::Cancelled,
                answers: Default::default(),
                comment: None,
                unanswered: request.questions.iter().map(|q| q.id.clone()).collect(),
                auto_resolved_after_ms: None,
                cancel_origin: None,
                resolved_by: None,
            })
        })
    }
    fn notify(&self, _: &str, _: NotificationType) {}
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {}
    fn set_header(&self, _: Option<ComponentFactory>) {}
    fn set_footer(&self, _: Option<ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> {
        Box::pin(async { Err(ExtensionFailure::new("No custom UI in this test")) })
    }
    fn theme(&self) -> Theme {
        Theme::default()
    }
}

fn question_args(tool: &str, header: &str, labels: [&str; 2]) -> Value {
    let options = labels
        .iter()
        .map(|label| json!({"label": label, "description": "One choice"}))
        .collect::<Vec<_>>();
    if tool == "ask_user_question" {
        json!({"waitForAnswer": true, "questions": [{
            "question": "Go ahead with the release?",
            "header": header,
            "multiSelect": false,
            "options": options,
        }]})
    } else {
        json!({"wait_for_answer": true, "questions": [{
            "id": "release",
            "question": "Go ahead with the release?",
            "header": header,
            "options": options,
        }]})
    }
}

/// Run one waiting question in a fresh session with the gate and ask-user extensions, let it time
/// out, and return the tool name, the arguments sent and the tool result.
async fn run_unanswered(
    header: &str,
    labels: [&str; 2],
) -> Result<(String, Value, maho_agent::types::AgentToolResult), TestError> {
    let root = tempfile::tempdir()?;
    let project = root.path();
    let provider = faux_provider(RegisterFauxProviderOptions {
        tokens_per_second: Some(0.0),
        ..Default::default()
    })?;
    let model = provider.get_model(Some("faux-1")).ok_or("Missing faux model")?;
    let streams = faux_streams(provider.core.clone());
    let runtime = maho_core::model_runtime::ModelRuntime::create_sync(
        maho_core::model_runtime::CreateModelRuntimeOptions {
            models_path: Some(project.join("models.json")),
            auth_path: Some(project.join("auth.json")),
            providers: Some(vec![provider.provider.clone()]),
            ..Default::default()
        },
    )?;
    let cwd = project.to_string_lossy().into_owned();
    let session = AgentSession::new(AgentSessionConfig {
        agent: maho_agent::Agent::new(maho_agent::AgentOptions {
            initial_state: Some(maho_agent::agent::PartialAgentState {
                model: Some(model),
                ..Default::default()
            }),
            stream_fn: Some(Arc::new(move |model, context, options| {
                streams.stream_simple(model, context, options.map(|options| options.simple))
            })),
            ..Default::default()
        }),
        session_manager: maho_core::session_manager::SessionManager::in_memory(&cwd, None, None),
        settings_manager: maho_core::settings_manager::SettingsManager::from_storage(
            Box::new(maho_core::settings_manager::InMemorySettingsStorage::default()),
            true,
        ),
        cwd: cwd.clone(),
        agent_dir: Some(cwd),
        fallback_now: Some(Arc::new(|| 0.0)),
        retry_random: Some(Arc::new(|| 0.5)),
        scoped_models: Vec::new(),
        favorite_models: Vec::new(),
        flag_values: Default::default(),
        custom_tools: Vec::new(),
        model_runtime: Some(runtime),
        model_registry: None,
        uses_default_stream_function: Some(false),
        initial_active_tool_names: None,
        default_tool_names: None,
        eval_only_tool_names: None,
        allowed_tool_names: None,
        excluded_tool_names: None,
        base_tools_override: None,
        session_start_event: None,
        auto_title_sessions: Some(false),
    })?;
    let loaded = load_extensions(
        vec![
            NativeExtensionFactory {
                path: "<builtin:approval-gates>".into(),
                source_info: SourceInfo::default(),
                extension: Box::new(maho_approval_gates::ApprovalGates),
            },
            NativeExtensionFactory {
                path: "<builtin:ask-user>".into(),
                source_info: SourceInfo::default(),
                extension: Box::new(AskUser),
            },
        ],
        project,
        ExtensionSessionProfile::default(),
    );
    if !loaded.errors.is_empty() {
        return Err(format!("Factory errors: {:?}", loaded.errors).into());
    }
    let ui: Arc<dyn ExtensionUi> = Arc::new(SilentUi);
    let mut context = session.extension_context(ui.clone());
    context.mode = ExtensionMode::Tui;
    context.has_ui = true;
    let runner = ExtensionRunner::new(loaded.extensions, loaded.runtime, loaded.events, context);
    session.set_extension_runner(runner).await;
    session
        .bind_extensions(ExtensionBindings {
            ui_context: Some(ui),
            mode: Some(ExtensionMode::Tui),
            ..Default::default()
        })
        .await;
    let outcome = async {
        let tool = session
            .get_active_tool_names()
            .into_iter()
            .find(|name| ["ask_user_question", "request_user_input"].contains(&name.as_str()))
            .ok_or("No active question tool")?;
        let args = question_args(&tool, header, labels);
        // Bound in virtual time: well past the 30-minute question timeout.
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(4 * 60 * 60),
            session.execute_tool(&tool, args.clone(), ExecuteToolOptions::default()),
        )
        .await??;
        Ok::<_, TestError>((tool, args, result))
    }
    .await;
    session.emit_session_shutdown(SessionReason::Quit).await;
    session.dispose().await;
    root.close()?;
    outcome
}

fn result_text(result: &maho_agent::types::AgentToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            maho_ai::types::ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect()
}

fn timed_out_response(request: &QuestionRequest) -> QuestionResponse {
    QuestionResponse {
        status: QuestionStatus::TimedOut,
        answers: Default::default(),
        comment: None,
        unanswered: request.questions.iter().map(|q| q.id.clone()).collect(),
        auto_resolved_after_ms: Some(request.timeout_ms),
        cancel_origin: None,
        resolved_by: None,
    }
}

fn canonical(tool: &str, args: &Value) -> Result<QuestionRequest, TestError> {
    let variant = if tool == "ask_user_question" {
        AskUserVariant::Claude
    } else {
        AskUserVariant::Codex
    };
    Ok(to_canonical(variant, args, "test".into(), None)?)
}

/// 14. A gated question that times out yields the builtin's "do not take the action" result; the
/// same question without a gate header or label yields the ordinary unanswered result.
#[tokio::test(start_paused = true)]
async fn timed_out_gate_yields_do_not_act_result() -> Result<(), TestError> {
    let (tool, args, gated) = run_unanswered("Approval", ["Ship", "Hold"]).await?;
    assert_eq!(gated.details["status"], json!("timed_out"));
    let request = canonical(&tool, &args)?;
    let response = timed_out_response(&request);
    assert_eq!(
        result_text(&gated),
        format_required_no_answer(&response, &request.questions)
    );

    let (tool, args, plain) = run_unanswered("Release", ["Ship", "Hold"]).await?;
    assert_eq!(plain.details["status"], json!("timed_out"));
    let request = canonical(&tool, &args)?;
    let response = timed_out_response(&request);
    assert_eq!(
        result_text(&plain),
        format_result_text(&response, &request.questions)
    );
    assert_ne!(result_text(&plain), result_text(&gated));
    Ok(())
}

/// 14 (label variant): a label-rule gate also turns the timeout into the "do not act" result.
#[tokio::test(start_paused = true)]
async fn timed_out_label_gate_yields_do_not_act_result() -> Result<(), TestError> {
    let (tool, args, gated) = run_unanswered("Release", ["Approve release", "Hold"]).await?;
    assert_eq!(gated.details["status"], json!("timed_out"));
    let request = canonical(&tool, &args)?;
    assert_eq!(
        result_text(&gated),
        format_required_no_answer(&timed_out_response(&request), &request.questions)
    );
    Ok(())
}
