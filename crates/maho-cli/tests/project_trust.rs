use maho_cli::cli::project_trust::{extension_mode, CliProjectTrustContext};
use maho_core::project_trust::{AppMode, ProjectTrustContext};
#[test]
fn mode_adapter_maps_app_server_to_print() {
    for (mode, expected) in [(AppMode::Interactive, "tui"), (AppMode::Print, "print"), (AppMode::AppServer, "print"), (AppMode::Json, "json"), (AppMode::Rpc, "rpc")] { assert_eq!(extension_mode(mode), expected); }
}
#[test]
fn selector_is_called_only_for_interactive_ui() {
    for mode in [AppMode::Interactive, AppMode::Print, AppMode::Json, AppMode::Rpc, AppMode::AppServer] {
        for available in [true, false] {
            let context = CliProjectTrustContext { cwd: "/project".to_owned(), mode, ui_available: available, selector: Box::new(|title, options| { assert_eq!(title, "Trust?"); Some(options[0].clone()) }) };
            assert_eq!(context.has_ui(), available);
            assert_eq!(context.select("Trust?", &["Yes".to_owned()]), (available && mode == AppMode::Interactive).then(|| "Yes".to_owned()));
        }
    }
}
