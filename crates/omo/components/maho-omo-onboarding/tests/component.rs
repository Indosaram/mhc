mod support;
use maho_ext_api::*;
use maho_omo_onboarding::component::{OnboardingComponent, OnboardingDependencies};
use std::sync::{Arc, Mutex};
#[derive(Default)]
struct Actions(Mutex<Vec<(CustomMessage, SendMessageOptions)>>);
impl ExtensionActions for Actions {
    fn send_message(
        &self,
        m: CustomMessage,
        o: SendMessageOptions,
    ) -> Result<(), ExtensionFailure> {
        self.0.lock().expect("messages").push((m, o));
        Ok(())
    }
    fn send_user_message(
        &self,
        _: UserMessageContent,
        _: SendUserMessageOptions,
    ) -> Result<(), ExtensionFailure> {
        panic!("unexpected user message")
    }
    fn append_entry(&self, _: &str, _: Option<JsonValue>) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn get_all_tools(&self) -> Result<Vec<ToolInfo>, ExtensionFailure> {
        Ok(vec![])
    }
}
fn register(path: &std::path::Path) -> (ExtensionApi, Arc<Actions>) {
    let actions = Arc::new(Actions::default());
    let runtime = ExtensionRuntime::default();
    runtime.bind(actions.clone());
    let mut api = ExtensionApi::new(
        LoadedExtension::new("onboarding", "/tmp".into(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        runtime,
    );
    OnboardingComponent {
        state_dir: path.into(),
        skills_root: "/skills".into(),
    }
    .with_dependencies(OnboardingDependencies {
        session_history: Some(Arc::new(|| None)),
        scan_environment: Arc::new({
            let dir = path.to_owned();
            move || support::hermetic_scan_environment(&dir)
        }),
        ..Default::default()
    })
    .register(&mut api);
    (api, actions)
}
async fn start_with(api: &ExtensionApi, reason: SessionReason, context: &ExtensionContext) {
    let mut event = ExtensionEvent::SessionStart(SessionStartEvent {
        reason,
        initial_model_provenance: None,
        previous_session_file: None,
        attended: None,
    });
    api.registered.handlers[&EventKind::SessionStart][0](&mut event, context)
        .await
        .expect("dispatch");
}
async fn start(api: &ExtensionApi, reason: SessionReason) {
    start_with(api, reason, &support::context()).await;
}
#[tokio::test]
async fn ordinary_startup_does_not_bootstrap_or_claim() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let (api, a) = register(root.path());
    start(&api, SessionReason::Startup).await;
    assert!(!maho_omo_onboarding::state::is_onboarding_complete(
        root.path()
    ));
    assert!(a.0.lock().expect("messages").is_empty());
    Ok(())
}
#[tokio::test]
async fn new_does_not_inject() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let (api, a) = register(root.path());
    start(&api, SessionReason::New).await;
    assert!(a.0.lock().expect("messages").is_empty());
    Ok(())
}
#[tokio::test]
async fn resume_reload_fork_do_not_inject() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let (api, a) = register(root.path());
    for reason in [
        SessionReason::Resume,
        SessionReason::Reload,
        SessionReason::Fork,
    ] {
        start(&api, reason).await;
    }
    assert!(a.0.lock().expect("messages").is_empty());
    Ok(())
}
#[tokio::test]
async fn disabled_does_not_inject() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let (api, a) = register(root.path());
    api.set_flag("omo-senpi-onboarding-disabled", FlagValue::Boolean(true));
    start(&api, SessionReason::Startup).await;
    assert!(a.0.lock().expect("messages").is_empty());
    Ok(())
}
#[tokio::test]
async fn force_only_once() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    maho_omo_onboarding::state::claim_onboarding(root.path());
    let (api, a) = register(root.path());
    api.set_flag("onboard", FlagValue::Boolean(true));
    start(&api, SessionReason::Startup).await;
    start(&api, SessionReason::Startup).await;
    assert_eq!(a.0.lock().expect("messages").len(), 1);
    Ok(())
}
#[tokio::test]
async fn claimed_does_not_inject() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    maho_omo_onboarding::state::claim_onboarding(root.path());
    let (api, a) = register(root.path());
    start(&api, SessionReason::Startup).await;
    assert!(a.0.lock().expect("messages").is_empty());
    Ok(())
}
#[tokio::test]
async fn headless_startup_does_not_claim_or_inject() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let (api, a) = register(root.path());
    start_with(
        &api,
        SessionReason::Startup,
        &support::context_with_ui(false),
    )
    .await;
    assert!(!maho_omo_onboarding::state::is_onboarding_complete(
        root.path()
    ));
    assert!(a.0.lock().expect("messages").is_empty());
    Ok(())
}
#[tokio::test]
async fn print_mode_startup_does_not_claim_or_inject() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let (api, a) = register(root.path());
    start_with(
        &api,
        SessionReason::Startup,
        &support::context_with_ui(true),
    )
    .await;
    assert!(!maho_omo_onboarding::state::is_onboarding_complete(
        root.path()
    ));
    assert!(a.0.lock().expect("messages").is_empty());
    Ok(())
}
#[tokio::test]
async fn worker_startup_does_not_claim_or_inject() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let mut api = register(root.path()).0;
    api.profile.session_kind = SessionKind::Worker;
    let mut event = ExtensionEvent::SessionStart(SessionStartEvent {
        reason: SessionReason::Startup,
        initial_model_provenance: None,
        previous_session_file: None,
        attended: None,
    });
    api.registered.handlers[&EventKind::SessionStart][0](&mut event, &support::context())
        .await
        .expect("dispatch");
    assert!(!maho_omo_onboarding::state::is_onboarding_complete(
        root.path()
    ));
    Ok(())
}
