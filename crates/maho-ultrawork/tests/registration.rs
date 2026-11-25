//! Runtime delivery through the registered handlers.

use maho_ext_api::{
    AgentMessage, BeforeAgentStartEvent, BuildSystemPromptOptions, ContextEventSource, EventBus,
    EventKind, EventResult, Extension, ExtensionApi, ExtensionContext, ExtensionEvent,
    ExtensionRuntime, ExtensionSessionProfile, InputEvent, InputEventResult, InputSource,
    LoadedExtension, Message, Model, SourceInfo, StreamingBehavior, ToolContent, UserContent,
    UserMessage,
};
use maho_ultrawork::{
    CORE_CONTRACT, DISABLE_FLAG, Engine, REMINDER_MESSAGE_TYPE, UltraworkExtension, mass_block,
};
use std::future::Future;
use std::task::{Context, Poll, Waker};

mod stub;

const GPT: &str = include_str!("../../../assets/omo-latest/skills/ultrawork/references/gpt.md");

fn registered() -> ExtensionApi {
    register(Engine::new(Default::default()))
}

fn register(engine: Engine) -> ExtensionApi {
    register_with_profile(engine, ExtensionSessionProfile::default())
}

fn register_with_profile(engine: Engine, profile: ExtensionSessionProfile) -> ExtensionApi {
    let mut api = ExtensionApi::new(
        LoadedExtension::new("ultrawork", "/tmp".into(), SourceInfo::default()),
        profile,
        EventBus::default(),
        ExtensionRuntime::default(),
    );
    UltraworkExtension::new(engine).register(&mut api);
    api
}

#[test]
fn arming_and_request_projection_handlers_are_registered() {
    let api = registered();
    let mut kinds: Vec<_> = api.registered.handlers.keys().copied().collect();
    kinds.sort();
    let mut expected = vec![
        EventKind::Input,
        EventKind::BeforeAgentStart,
        EventKind::Context,
        EventKind::SessionCompact,
        EventKind::ToolResult,
    ];
    expected.sort();
    assert_eq!(kinds, expected);
}

#[test]
fn disable_flag_is_registered_as_boolean_defaulting_off() {
    let api = registered();
    assert!(
        api.registered
            .flags
            .iter()
            .any(|flag| flag.name == DISABLE_FLAG)
    );
    assert_eq!(
        api.get_flag(DISABLE_FLAG),
        Some(maho_ext_api::FlagValue::Boolean(false))
    );
}

/// Drives a future that never waits on anything external.
fn run<T>(future: impl Future<Output = T>) -> T {
    let mut future = std::pin::pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    match future.as_mut().poll(&mut cx) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("handler future was not immediately ready"),
    }
}

#[test]
fn three_hundred_turns_deliver_one_directive_through_the_registered_handlers() {
    let engine = Engine::new(Default::default());
    let api = register(engine.clone());
    let ctx: ExtensionContext = stub::context("registration-300");
    let handlers = &api.registered.handlers;
    let mut directives = 0_usize;
    for turn in 0..300 {
        let text = if turn == 0 { "ulw go" } else { "continue" };
        let mut input = ExtensionEvent::Input(InputEvent {
            input_id: format!("i{turn}"),
            text: text.into(),
            images: None,
            source: InputSource::Interactive,
            streaming_behavior: None,
        });
        for handler in &handlers[&EventKind::Input] {
            run(handler(&mut input, &ctx)).expect("input handler");
        }
        let mut start = ExtensionEvent::BeforeAgentStart(BeforeAgentStartEvent {
            prompt: text.into(),
            trigger: "prompt".into(),
            preview: false,
            images: None,
            system_prompt: String::new(),
            system_prompt_options: BuildSystemPromptOptions::default(),
        });
        for handler in &handlers[&EventKind::BeforeAgentStart] {
            if let EventResult::BeforeAgentStart(result) =
                run(handler(&mut start, &ctx)).expect("start handler")
                && result.message.is_some()
            {
                directives += 1;
            }
        }
    }
    assert_eq!(directives, 1);
    assert!(engine.sessions().snapshot("registration-300").armed);
}

fn model(id: &str) -> Model {
    serde_json::from_value(serde_json::json!({
        "id": id, "name": id, "provider": "openai", "api": "openai-completions",
        "baseUrl": "https://example.invalid", "reasoning": false, "input": ["text"],
        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
        "contextWindow": 8192, "maxTokens": 2048
    }))
    .expect("model fixture")
}

fn input(
    api: &ExtensionApi,
    ctx: &ExtensionContext,
    text: &str,
    streaming_behavior: Option<StreamingBehavior>,
) -> InputEventResult {
    let mut event = ExtensionEvent::Input(InputEvent {
        input_id: "i".into(),
        text: text.into(),
        images: None,
        source: InputSource::Interactive,
        streaming_behavior,
    });
    let EventResult::Input(result) = run(api.registered.handlers[&EventKind::Input][0](
        &mut event, ctx,
    ))
    .expect("input") else {
        panic!("expected input result");
    };
    result
}

fn start(api: &ExtensionApi, ctx: &ExtensionContext) -> Option<maho_ext_api::CustomMessage> {
    let mut event = ExtensionEvent::BeforeAgentStart(BeforeAgentStartEvent {
        prompt: "ulw verify".into(),
        trigger: "prompt".into(),
        preview: false,
        images: None,
        system_prompt: String::new(),
        system_prompt_options: Default::default(),
    });
    match run(api.registered.handlers[&EventKind::BeforeAgentStart][0](
        &mut event, ctx,
    ))
    .expect("start")
    {
        EventResult::BeforeAgentStart(result) => result.message,
        EventResult::None => None,
        _ => panic!("expected turn start result"),
    }
}

fn project(api: &ExtensionApi, ctx: &ExtensionContext, message: &AgentMessage) -> AgentMessage {
    project_from(api, ctx, message, ContextEventSource::Agent)
}

fn project_from(
    api: &ExtensionApi,
    ctx: &ExtensionContext,
    message: &AgentMessage,
    source: ContextEventSource,
) -> AgentMessage {
    let mut event = ExtensionEvent::Context {
        messages: vec![message.clone()],
        entry_ids: vec![Some("entry".into())],
        source,
    };
    run(api.registered.handlers[&EventKind::Context][0](
        &mut event, ctx,
    ))
    .expect("context");
    let ExtensionEvent::Context {
        mut messages,
        entry_ids,
        ..
    } = event
    else {
        panic!("expected context event");
    };
    assert_eq!(entry_ids, [Some("entry".to_owned())]);
    messages.pop().expect("projected message")
}

fn user(text: &str) -> AgentMessage {
    Message::User(UserMessage {
        content: UserContent::Text(text.into()),
        timestamp: 42,
    })
    .into()
}

#[test]
fn idle_directive_uses_the_turn_model_and_matches_pinned_model_ids() {
    for (id, expected) in [
        (None, CORE_CONTRACT),
        (Some("gateway/openai/Fast-GPT-6.2:high"), GPT),
        (Some("gpt-gateway/claude-fable-5-2"), CORE_CONTRACT),
        (Some("deepseek-flash"), GPT),
        (Some("gateway/DeepSeek-V4.1-Flash-20261101"), GPT),
        (Some("deepseek-v4p1-flash@thinking"), GPT),
        (Some("deepseek-v4-1-flash:fast"), GPT),
        (Some("deepseek-v4-flash-0731"), CORE_CONTRACT),
        (Some("deepseek-v4.1-pro"), CORE_CONTRACT),
        (Some("deepseek-v4p1-flashlight"), CORE_CONTRACT),
    ] {
        let api = registered();
        let mut ctx = stub::context("idle");
        assert_eq!(
            input(&api, &ctx, "ulw verify", None),
            InputEventResult::Continue
        );
        ctx.model = id.map(model);
        let message = start(&api, &ctx).expect("hidden directive");
        assert!(!message.display);
        assert_eq!(
            message.content,
            [ToolContent::text(format!(
                "<omo-ultrawork-directive>\n{expected}\n</omo-ultrawork-directive>"
            ))],
            "{id:?}"
        );
    }
}

#[test]
fn queued_mass_directive_changes_with_the_receiving_model_without_rewriting_saved_text() {
    let api = registered();
    let mut ctx = stub::context("queued");
    ctx.model = Some(model("claude-fable-5-2"));
    let prompt = "mass ulw verify";
    let InputEventResult::Transform { text, .. } =
        input(&api, &ctx, prompt, Some(StreamingBehavior::FollowUp))
    else {
        panic!("expected queued directive");
    };
    let original = user(&text);
    let expected = |body: &str| {
        user(&format!(
            "{prompt}\n\n<omo-ultrawork-directive>\n{body}\n</omo-ultrawork-directive>\n\n{}",
            mass_block()
        ))
    };
    assert_eq!(original, expected(CORE_CONTRACT));
    ctx.model = Some(model("gpt-6.2"));
    assert_eq!(project(&api, &ctx, &original), expected(GPT));
    ctx.model = Some(model("claude-fable-5-2"));
    assert_eq!(project(&api, &ctx, &original), expected(CORE_CONTRACT));
}

#[test]
fn replayed_custom_directive_restores_arming_even_without_reselection() {
    let message = |body: &str| -> AgentMessage {
        serde_json::from_value(serde_json::json!({
            "role": "custom", "customType": "omo-ultrawork:directive", "display": false,
            "timestamp": 42,
            "content": [{"type":"text", "text": format!("<omo-ultrawork-directive>\n{body}\n</omo-ultrawork-directive>")}]
        }))
        .expect("replayed custom message")
    };
    for body in [CORE_CONTRACT, GPT] {
        let api = registered();
        let ctx = stub::context("replay");
        assert_eq!(project(&api, &ctx, &message(body)), message(CORE_CONTRACT));
        assert_eq!(
            input(&api, &ctx, "ulw verify", None),
            InputEventResult::Continue
        );
        assert_eq!(
            start(&api, &ctx).expect("re-mention notice").custom_type,
            REMINDER_MESSAGE_TYPE
        );
    }
}

#[test]
fn preview_child_and_compaction_contexts_do_not_restore_arming() {
    let directive = user(CORE_CONTRACT);
    for (source, child, compacting, compacted) in [
        (ContextEventSource::Projection, false, false, false),
        (ContextEventSource::Agent, true, false, false),
        (ContextEventSource::Agent, false, true, false),
        (ContextEventSource::Agent, false, false, true),
    ] {
        let engine = Engine::new(Default::default());
        let mut profile = ExtensionSessionProfile::default();
        if child {
            profile
                .session_context
                .insert("role".into(), "member".into());
        }
        let api = register_with_profile(engine.clone(), profile);
        let mut ctx = stub::context("protected");
        ctx.is_compacting_fn = std::sync::Arc::new(move || compacting);
        if compacted {
            engine.sessions().mark_armed("protected");
            engine.on_compaction("protected");
        }
        let before = engine.sessions().snapshot("protected");
        project_from(&api, &ctx, &directive, source);
        assert_eq!(engine.sessions().snapshot("protected"), before);
    }
}

#[test]
fn explicit_skill_projects_its_inline_contract_without_duplicate_injection() {
    let api = registered();
    let mut ctx = stub::context("explicit");
    ctx.model = Some(model("deepseek-v4.1-flash"));
    assert_eq!(
        input(&api, &ctx, "/skill:ultrawork verify", None),
        InputEventResult::Continue
    );
    assert!(start(&api, &ctx).is_none());
    let skill = include_str!("../../../assets/omo-latest/skills/ultrawork/SKILL.md");
    let (_, body) = skill[4..].split_once("\n---\n").expect("skill body");
    let expanded = |body: &str| {
        user(&format!(
            "<skill-instruction name=\"ultrawork\" location=\"/skills/ultrawork/SKILL.md\">\nReferences are relative to /skills/ultrawork.\n\n{}\n</skill-instruction>\n\nverify",
            body.trim()
        ))
    };
    let tail = body
        .trim()
        .strip_prefix(CORE_CONTRACT.trim())
        .expect("canonical default skill");
    assert_eq!(
        project(&api, &ctx, &expanded(body)),
        expanded(&format!("{}{tail}", GPT.trim_end()))
    );
}
