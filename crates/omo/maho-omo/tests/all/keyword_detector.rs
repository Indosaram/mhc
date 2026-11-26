use std::fs;
use std::path::Path;
use std::sync::Arc;

use maho_ext_api::{
    BeforeAgentStartEvent, EventBus, EventKind, EventResult, Extension, ExtensionApi,
    ExtensionEvent, ExtensionRuntime, ExtensionSessionProfile, InputEvent, InputSource,
    LoadedExtension, SourceInfo,
};
use maho_omo::{OmoComponentOptions, OmoExtension, OmoSenpiComponent, ProvisioningOptions};

use crate::settle_observers::test_context;
use crate::support::set_boolean_flag;

fn write_config(root: &Path, document: &str) {
    let dir = root.join(".omo");
    fs::create_dir_all(&dir).expect("create isolated config directory");
    fs::write(dir.join("omo.jsonc"), document).expect("write isolated JSONC");
}

async fn receives_directive(user: Option<&str>, project: Option<&str>, flag: Option<&str>) -> bool {
    let temp = tempfile::tempdir().expect("create isolated config root");
    let home = temp.path().join("home");
    let cwd = home.join("project");
    fs::create_dir_all(&cwd).expect("create project");
    if let Some(user) = user {
        write_config(&home, user);
    }
    if let Some(project) = project {
        write_config(&cwd, project);
    }
    let options = OmoComponentOptions {
        skills_root: temp.path().join("skills"),
        state_dir: temp.path().join("state"),
        env: [
            ("HOME".into(), home.to_string_lossy().into_owned()),
            ("OMO_PROFILE".into(), "test".into()),
        ]
        .into(),
        facts_read_only_bootstrap: false,
    };
    let mut components = maho_omo::omo_components(
        OmoSenpiComponent::from_register("task", |_| {}),
        OmoSenpiComponent::from_register("memory", |_| {}),
        &options,
    );
    // Use the shipped slot, but don't start unrelated onboarding, memory or tool components.
    components.retain(|component| component.name == "ultrawork");
    assert_eq!(components.len(), 1);
    let extension = OmoExtension::with_options(
        components,
        Default::default(),
        ProvisioningOptions {
            toolkit_base_dir: Some(temp.path().join("toolkit")),
            dag_sdk_base_dir: Some(temp.path().join("dag")),
            env: Some(Arc::new(|_| None)),
            ..Default::default()
        },
    );
    let mut api = ExtensionApi::new(
        LoadedExtension::new("omo", cwd.clone(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        ExtensionRuntime::default(),
    );
    if let Some(flag) = flag {
        set_boolean_flag(&mut api, flag, true);
    }
    extension.register(&mut api);
    let session_id = temp.path().to_string_lossy();
    let mut context = test_context(&session_id);
    context.cwd = cwd;
    context.agent_dir = home.join(".omo/agent");

    let prompt = "ulw review the session export";
    let mut input = ExtensionEvent::Input(InputEvent {
        input_id: "keyword-policy".into(),
        text: prompt.into(),
        images: None,
        source: InputSource::Interactive,
        streaming_behavior: None,
    });
    for handler in &api.registered.handlers[&EventKind::Input] {
        handler(&mut input, &context).await.expect("dispatch input");
    }
    let mut start = ExtensionEvent::BeforeAgentStart(BeforeAgentStartEvent {
        prompt: prompt.into(),
        trigger: "prompt".into(),
        preview: false,
        images: None,
        system_prompt: String::new(),
        system_prompt_options: Default::default(),
    });
    for handler in &api.registered.handlers[&EventKind::BeforeAgentStart] {
        if let EventResult::BeforeAgentStart(result) =
            handler(&mut start, &context).await.expect("dispatch start")
            && result.message.is_some()
        {
            return true;
        }
    }
    false
}

#[tokio::test]
async fn registered_input_obeys_keyword_lists_without_changing_the_default() {
    for (name, project, expected) in [
        ("default", None, true),
        (
            "unrelated denial",
            Some(r#"{"keyword_detector":{"disabled_keywords":["hyperplan"]}}"#),
            true,
        ),
        (
            "explicit allow",
            Some(r#"{"keyword_detector":{"enabled_expansions":["ultrawork"]}}"#),
            true,
        ),
        (
            "explicit deny",
            Some(r#"{"keyword_detector":{"disabled_keywords":["ultrawork"]}}"#),
            false,
        ),
        (
            "native allowlist omits ultrawork",
            Some(r#"{"[native]":{"keyword_detector":{"enabled_expansions":["team"]}}}"#),
            false,
        ),
        (
            "empty allowlist",
            Some(r#"{"keyword_detector":{"enabled_expansions":[]}}"#),
            false,
        ),
        (
            "deny wins",
            Some(
                r#"{"keyword_detector":{"enabled_expansions":["ultrawork"],"disabled_keywords":["ultrawork"]}}"#,
            ),
            false,
        ),
    ] {
        assert_eq!(
            receives_directive(None, project, None).await,
            expected,
            "{name}"
        );
    }
}

#[tokio::test]
async fn project_profile_can_replace_user_lists_and_a_launch_flag_still_wins() {
    let user = r#"{"keyword_detector":{"disabled_keywords":["ultrawork"]}}"#;
    assert!(!receives_directive(Some(user), None, None).await);
    let project = r#"{
        // Only the selected native profile clears the user denylist.
        "profiles": {
            "test": {
                "[native]": {
                    "keyword_detector": { "disabled_keywords": [] },
                },
            },
        },
    }"#;
    assert!(receives_directive(Some(user), Some(project), None).await);
    for flag in [maho_ultrawork::DISABLE_FLAG, "omo-senpi-ultrawork-disabled"] {
        assert!(
            !receives_directive(Some(user), Some(project), Some(flag)).await,
            "{flag}"
        );
    }
}
