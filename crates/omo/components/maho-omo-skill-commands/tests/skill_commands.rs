mod support;

use std::path::Path;
use std::sync::Arc;

use maho_ext_api::*;
use maho_ext_host::ExtensionRunner;
use maho_omo_skill_commands::{SkillCommandsComponent, SkillCommandsComponentOptions};

const LOADED: [(&str, Option<&str>, &str); 4] = [
    ("tasks", None, "extension"),
    ("skill:ulw-execute", Some("Executes a work plan."), "skill"),
    ("skill:ulw-plan", Some("Plans first."), "skill"),
    ("skill:ulw-loop", Some("Goal loop."), "skill"),
];

fn loaded_commands() -> Vec<SlashCommandInfo> {
    LOADED.iter().map(|(name, description, source)| support::command(name, *description, source)).collect()
}

fn component(skills_dir: &Path) -> SkillCommandsComponent {
    SkillCommandsComponent::new(SkillCommandsComponentOptions {
        skills_dir: Some(skills_dir.to_path_buf()),
        env: Some(std::collections::BTreeMap::new()),
        logger: None,
    })
}

fn runner(skills_dir: &Path, commands: Option<Vec<SlashCommandInfo>>, ui: Arc<support::RecordingUi>, has_ui: bool) -> ExtensionRunner {
    let runner = ExtensionRunner::from_static(vec![Box::new(component(skills_dir))], support::context(skills_dir, ui, has_ui));
    if let Some(commands) = commands {
        runner.bind_session_actions(Arc::new(support::FakeSessionActions { commands })).expect("bind session actions");
    }
    runner
}

async fn submit(runner: &mut ExtensionRunner, text: &str, source: InputSource, images: Option<Vec<ImageContent>>) -> InputEventResult {
    runner
        .emit_input(InputEvent { input_id: "id".to_owned(), text: text.to_owned(), images, source, streaming_behavior: None })
        .await
        .expect("input dispatch")
}

fn transform(text: &str) -> InputEventResult {
    InputEventResult::Transform { text: text.to_owned(), images: None }
}

#[tokio::test]
async fn given_ulw_execute_is_loaded_when_the_user_submits_a_bare_command_then_it_becomes_the_skill_form_with_args_intact() {
    let fixture = support::skills_fixture(&["ulw-execute", "ulw-plan", "ulw-loop"]);
    let mut runner = runner(&fixture.skills, Some(loaded_commands()), Arc::new(support::RecordingUi::default()), false);

    assert_eq!(
        submit(&mut runner, "/ulw-execute demo-plan --make-pr", InputSource::Interactive, None).await,
        transform("/skill:ulw-execute demo-plan --make-pr")
    );
    assert_eq!(submit(&mut runner, "/ulw-plan", InputSource::Interactive, None).await, transform("/skill:ulw-plan"));
    assert_eq!(
        submit(&mut runner, "/ulw-loop fix it\nsecond line", InputSource::Interactive, None).await,
        transform("/skill:ulw-loop fix it\nsecond line")
    );
}

#[tokio::test]
async fn given_an_attached_image_when_a_bare_skill_command_is_rewritten_then_the_image_rides_along() {
    let fixture = support::skills_fixture(&["ulw-plan"]);
    let mut runner = runner(&fixture.skills, Some(loaded_commands()), Arc::new(support::RecordingUi::default()), false);
    let image = ImageContent { data: "AA==".to_owned(), mime_type: "image/png".to_owned() };

    assert_eq!(
        submit(&mut runner, "/ulw-plan this screen", InputSource::Interactive, Some(vec![image.clone()])).await,
        InputEventResult::Transform { text: "/skill:ulw-plan this screen".to_owned(), images: Some(vec![image]) }
    );
}

#[tokio::test]
async fn given_inputs_that_do_not_name_a_bundled_skill_command_when_submitted_then_they_pass_through_untouched() {
    let fixture = support::skills_fixture(&["ulw-execute"]);
    let mut runner = runner(&fixture.skills, Some(loaded_commands()), Arc::new(support::RecordingUi::default()), false);

    for text in [
        "/ulw-executex plan",
        "/foo bar",
        " /ulw-execute plan",
        "run /ulw-execute plan",
        "/skill:ulw-execute plan",
        "/tasks",
    ] {
        assert_eq!(submit(&mut runner, text, InputSource::Interactive, None).await, InputEventResult::Continue, "{text}");
    }
    assert_eq!(
        submit(&mut runner, "/ulw-execute plan", InputSource::Extension, None).await,
        InputEventResult::Continue
    );
}

#[tokio::test]
async fn given_a_user_prompt_template_or_another_command_owns_the_name_when_submitted_then_the_alias_steps_aside() {
    let fixture = support::skills_fixture(&["refactor"]);
    let mut commands = loaded_commands();
    commands.push(support::command("refactor", None, "prompt"));
    commands.push(support::command("skill:refactor", None, "skill"));
    let mut runner = runner(&fixture.skills, Some(commands), Arc::new(support::RecordingUi::default()), false);

    assert_eq!(submit(&mut runner, "/refactor src", InputSource::Interactive, None).await, InputEventResult::Continue);
}

#[tokio::test]
async fn given_the_bundled_skill_is_disabled_when_its_bare_command_is_submitted_then_nothing_reaches_the_model_and_a_warning_names_the_skill() {
    let fixture = support::skills_fixture(&["ulw-research"]);
    let ui = Arc::new(support::RecordingUi::default());
    let mut runner = runner(&fixture.skills, Some(loaded_commands()), ui.clone(), true);

    assert_eq!(submit(&mut runner, "/ulw-research why", InputSource::Interactive, None).await, InputEventResult::Handled);

    let notices = ui.notices();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].1, NotificationType::Warning);
    assert!(notices[0].0.contains("ulw-research"), "the warning names the skill: {}", notices[0].0);
}

#[tokio::test]
async fn given_a_host_without_get_commands_when_a_bare_skill_command_is_submitted_then_it_is_still_rewritten() {
    let fixture = support::skills_fixture(&["ulw-execute"]);
    let mut runner = runner(&fixture.skills, None, Arc::new(support::RecordingUi::default()), false);

    assert_eq!(
        submit(&mut runner, "/ulw-execute plan", InputSource::Interactive, None).await,
        transform("/skill:ulw-execute plan")
    );
}

#[test]
fn given_no_bundled_skills_directory_when_registered_then_no_handler_is_installed() {
    let root = tempfile::tempdir().expect("temp");
    let missing = root.path().join("omo-skill-commands-missing");
    let mut api = ExtensionApi::new(
        LoadedExtension::new("skill-commands", root.path().to_path_buf(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        ExtensionRuntime::default(),
    );

    component(&missing).register(&mut api);

    assert!(api.registered.handlers.is_empty());
}

#[tokio::test]
async fn given_a_ui_when_the_session_starts_then_the_bare_skill_autocomplete_provider_is_registered() {
    let fixture = support::skills_fixture(&["ulw-execute"]);
    let ui = Arc::new(support::RecordingUi::default());
    let mut api = ExtensionApi::new(
        LoadedExtension::new("skill-commands", fixture.skills.clone(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        ExtensionRuntime::default(),
    );
    component(&fixture.skills).register(&mut api);

    let mut event = ExtensionEvent::SessionStart(SessionStartEvent {
        reason: SessionReason::Startup,
        initial_model_provenance: None,
        previous_session_file: None,
    });
    api.registered.handlers[&EventKind::SessionStart][0](&mut event, &support::context(&fixture.skills, ui.clone(), true))
        .await
        .expect("session start");

    assert_eq!(ui.autocomplete_factory_count(), 1);
}

#[tokio::test]
async fn given_the_real_component_order_when_a_bare_skill_is_submitted_then_ultrawork_does_not_arm_while_plain_ulw_still_does() {
    let fixture = support::skills_fixture(&["ulw-execute", "ulw-loop"]);
    let ui = Arc::new(support::RecordingUi::default());
    let actions = Arc::new(support::RecordingActions::default());
    let ultrawork = maho_omo_ultrawork::UltraworkComponent {
        arming: Arc::new(std::sync::Mutex::new(maho_omo_ultrawork::SessionArming::default())),
    };
    let mut runner = ExtensionRunner::from_static(
        vec![Box::new(component(&fixture.skills)), Box::new(ultrawork)],
        support::context(&fixture.skills, ui, false),
    );
    runner.bind_session_actions(Arc::new(support::FakeSessionActions { commands: loaded_commands() })).expect("bind session actions");
    runner.runtime.bind(actions.clone());

    assert_eq!(
        submit(&mut runner, "/ulw-execute demo-plan", InputSource::Interactive, None).await,
        transform("/skill:ulw-execute demo-plan")
    );
    assert_eq!(
        submit(&mut runner, "/ulw-loop make hello.txt", InputSource::Interactive, None).await,
        transform("/skill:ulw-loop make hello.txt")
    );
    assert!(actions.0.lock().expect("messages").is_empty());

    assert_eq!(submit(&mut runner, "ulw make hello.txt", InputSource::Interactive, None).await, InputEventResult::Continue);
    let messages = actions.0.lock().expect("messages");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].custom_type, "omo-ultrawork:directive");
}
