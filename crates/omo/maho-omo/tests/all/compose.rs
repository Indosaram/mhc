use std::sync::{Arc, Mutex};
use maho_ext_api::{Extension, ExtensionApi, FlagValue};
use maho_omo::{LogLevel, OmoExtension, OmoSenpiComponent, OMO_DISABLED_FLAG, RecordingLogger};
use crate::support::{FakeComponent, command_names, manual_runtime_options, new_api};

#[test]
fn enabled_components_preserve_tools_commands_and_lifecycle_handlers() {
    let logger=Arc::new(RecordingLogger::new());
    let extension=OmoExtension::with_options(vec![
        OmoSenpiComponent::new("alpha",Box::new(FakeComponent::new(|api| {
            api.register_tool(crate::support::fake_tool("alpha_tool"));
            api.on(maho_ext_api::EventKind::SessionStart,Arc::new(|_,_|Box::pin(async {Ok(maho_ext_api::EventResult::None)})));
        }))),
        OmoSenpiComponent::new("beta",Box::new(FakeComponent::new(|api| {
            api.register_command("beta",None,None,Arc::new(|_,_|Box::pin(async {Ok(())})));
        }))),
    ],manual_runtime_options(logger),Default::default());
    let mut api=new_api();extension.register(&mut api);
    assert_eq!(crate::support::tool_names(&api),["alpha_tool"]);
    assert_eq!(command_names(&api),["beta"]);
    assert_eq!(api.registered.handlers[&maho_ext_api::EventKind::SessionStart].len(),1);
    let runtime=extension.runtime().expect("runtime");
    assert_eq!(runtime.captured_tools()[0].name,"alpha_tool");
    assert_eq!(runtime.context().get_flag(OMO_DISABLED_FLAG),Some(FlagValue::Boolean(false)));
}

#[test]
fn top_level_disable_skips_components_and_logs_once() {
    let logger=Arc::new(RecordingLogger::new());
    let extension=OmoExtension::with_options(vec![OmoSenpiComponent::new("alpha",Box::new(FakeComponent::new(|_|panic!("disabled component"))))],manual_runtime_options(logger.clone()),Default::default());
    let mut api=new_api();api.set_flag(OMO_DISABLED_FLAG,FlagValue::Boolean(true));extension.register(&mut api);
    assert!(extension.runtime().is_none());assert!(api.registered.tools.is_empty());
    let entries=logger.entries();assert_eq!(entries.len(),1);assert_eq!(entries[0].level,LogLevel::Info);
}

#[test]
fn disabled_env_skips_components_like_the_flag() {
    let logger=Arc::new(RecordingLogger::new());
    let provisioning=maho_omo::ProvisioningOptions{env:Some(Arc::new(|name:&str|(name==maho_omo::OMO_SENPI_DISABLED_ENV).then(||"1".to_owned()))),..Default::default()};
    let extension=OmoExtension::with_options(vec![OmoSenpiComponent::new("alpha",Box::new(FakeComponent::new(|_|panic!("disabled component"))))],manual_runtime_options(logger),provisioning);
    let mut api=new_api();extension.register(&mut api);
    assert!(extension.runtime().is_none());assert!(api.registered.tools.is_empty());
}

#[test]
fn components_receive_one_runtime_and_see_earlier_captured_tools() {
    let shared = Arc::new(Mutex::new(Vec::new()));
    let first = shared.clone();
    let second = shared.clone();
    let extension = OmoExtension::new(vec![
        OmoSenpiComponent::from_context_register("first", move |api, runtime| {
            first.lock().unwrap().push(runtime.context().idle_coordinator.clone());
            api.register_tool(crate::support::fake_tool("first"));
        }),
        OmoSenpiComponent::from_context_register("second", move |_, runtime| {
            assert_eq!(runtime.captured_tools()[0].name, "first");
            second.lock().unwrap().push(runtime.context().idle_coordinator.clone());
        }),
    ]);
    let mut api = crate::support::new_api();
    extension.register(&mut api);
    let shared = shared.lock().unwrap();
    assert_eq!(shared.len(), 2);
    assert!(Arc::ptr_eq(&shared[0], &shared[1]));
}

#[test]
fn disabled_reregistration_clears_the_previous_runtime() {
    let extension = OmoExtension::new(Vec::new());
    let mut api = crate::support::new_api();
    extension.register(&mut api);
    assert!(extension.runtime().is_some());
    api.set_flag(OMO_DISABLED_FLAG, FlagValue::Boolean(true));
    extension.register(&mut api);
    assert!(extension.runtime().is_none());
}

#[test]
fn a_failed_component_registration_keeps_its_error_identity_without_duplicating_the_mount() {
    let recording = Arc::new(RecordingLogger::new());
    let extension = OmoExtension::with_options(
        vec![
            OmoSenpiComponent::new(
                "boom",
                Box::new(FakeComponent::new(|_: &mut ExtensionApi| panic!("boom registration failed"))),
            ),
            OmoSenpiComponent::new(
                "survivor",
                Box::new(FakeComponent::new(|api: &mut ExtensionApi| {
                    api.register_command("survivor", Some("slot".to_owned()), None, Arc::new(|_, _| Box::pin(async { Ok(()) })));
                })),
            ),
        ],
        manual_runtime_options(recording.clone()),
        Default::default(),
    );

    let mut api = new_api();
    extension.register(&mut api);

    // The failed component keeps its own identity in the structured log...
    let errors: Vec<_> = recording.entries().into_iter().filter(|entry| entry.level == LogLevel::Error).collect();
    assert_eq!(errors.len(), 1, "exactly one registration failure logged");
    let details = errors[0].details.as_ref().expect("structured failure details");
    assert_eq!(details["component"], "boom");
    assert_eq!(details["error"], "boom registration failed");

    // ...the loop continues past it, and the single retained runtime is the only mount.
    assert_eq!(command_names(&api), vec!["survivor".to_owned()]);
    let first = extension.runtime().expect("one retained runtime after a failed component");
    extension.register(&mut api);
    let second = extension.runtime().expect("still the same retained runtime");
    assert!(Arc::ptr_eq(&first, &second), "a failed component must not duplicate the mount");
}
