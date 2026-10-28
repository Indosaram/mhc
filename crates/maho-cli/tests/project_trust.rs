use maho_cli::cli::project_trust::{extension_mode, CliProjectTrustContext};
use maho_core::project_trust::{AppMode, ProjectTrustContext};
#[test]
fn mode_adapter_maps_supported_modes() {
    for (mode, expected) in [(AppMode::Interactive, "tui"), (AppMode::Print, "print"), (AppMode::Json, "json"), (AppMode::Rpc, "rpc")] { assert_eq!(extension_mode(mode), expected); }
}
#[test]
fn selector_is_called_only_for_interactive_ui() {
    for mode in [AppMode::Interactive, AppMode::Print, AppMode::Json, AppMode::Rpc] {
        for available in [true, false] {
            let context = CliProjectTrustContext { cwd: "/project".to_owned(), mode, ui_available: available, selector: Box::new(|title, options| { assert_eq!(title, "Trust?"); Some(options[0].clone()) }) };
            assert_eq!(context.has_ui(), available);
            assert_eq!(context.select("Trust?", &["Yes".to_owned()]), (available && mode == AppMode::Interactive).then(|| "Yes".to_owned()));
        }
    }
}

struct TrustHandler(Option<maho_ext_api::TrustDecision>);
impl maho_ext_api::Extension for TrustHandler {
    fn register(&self, api: &mut maho_ext_api::ExtensionApi) {
        let decision = self.0;
        api.on(maho_ext_api::EventKind::ProjectTrust, std::sync::Arc::new(move |_, _| Box::pin(async move {
            match decision {
                Some(trusted) => Ok(maho_ext_api::EventResult::ProjectTrust(maho_ext_api::ProjectTrustEventResult { trusted, remember: Some(true) })),
                None => Err(maho_ext_api::ExtensionFailure::new("boom")),
            }
        })));
    }
}

fn trust_factory(name: &str, decision: Option<maho_ext_api::TrustDecision>) -> maho_ext_host::loader::NativeExtensionFactory {
    maho_ext_host::loader::NativeExtensionFactory { path: name.to_owned(), source_info: Default::default(), extension: Box::new(TrustHandler(decision)) }
}

fn headless_ui(mode: AppMode, has_ui: bool) -> std::sync::Arc<maho_cli::cli::project_trust::ProjectTrustUi> {
    std::sync::Arc::new(maho_cli::cli::project_trust::ProjectTrustUi {
        mode, has_ui, select: std::sync::Arc::new(|_: &str, options: &[String]| options.first().cloned()), input: std::sync::Arc::new(|_: &str, _: Option<&str>| Some("typed".to_owned())),
    })
}

#[tokio::test]
async fn first_decided_extension_wins_and_failures_become_warnings() {
    let root = tempfile::tempdir().expect("trust cwd");
    let loaded = maho_ext_host::loader::load_extensions(vec![
        trust_factory("<inline:failing>", None),
        trust_factory("<inline:undecided>", Some(maho_ext_api::TrustDecision::Undecided)),
        trust_factory("<inline:deny>", Some(maho_ext_api::TrustDecision::No)),
        trust_factory("<inline:allow>", Some(maho_ext_api::TrustDecision::Yes)),
    ], root.path(), Default::default());
    let cwd = root.path().to_string_lossy();
    let context = maho_cli::cli::project_trust::project_trust_extension_context(&cwd, &cwd, AppMode::Print, headless_ui(AppMode::Print, false));
    let (result, warnings) = maho_cli::cli::project_trust::emit_project_trust_event(loaded, context).await;
    assert_eq!(result, Some(maho_ext_api::ProjectTrustEventResult { trusted: maho_ext_api::TrustDecision::No, remember: Some(true) }));
    assert_eq!(warnings, ["Extension \"<inline:failing>\" project_trust error: boom"]);
}

#[tokio::test]
async fn trust_ui_answers_dialogs_only_in_an_interactive_ui() {
    use maho_ext_api::ExtensionUi;
    let options = ["Yes".to_owned(), "No".to_owned()];
    let interactive = headless_ui(AppMode::Interactive, true);
    assert_eq!(interactive.select("Trust?", &options, Default::default()).await.as_deref(), Some("Yes"));
    assert!(interactive.confirm("Trust?", "project", Default::default()).await);
    assert_eq!(interactive.input("Name", None, Default::default()).await.as_deref(), Some("typed"));
    for ui in [headless_ui(AppMode::Print, true), headless_ui(AppMode::Interactive, false)] {
        assert_eq!(ui.select("Trust?", &options, Default::default()).await, None);
        assert!(!ui.confirm("Trust?", "project", Default::default()).await);
        assert_eq!(ui.input("Name", None, Default::default()).await, None);
    }
}
