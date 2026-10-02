use maho_interactive::{extension_error_format::*, keybindings_command::*, loaded_resource_scopes::*, login_outcome::*, startup_tools::*, tmux_setup::*};
use maho_core::{keybindings::KeybindingsManager, source_info::{SourceInfo, SourceOrigin, SourceScope}};

#[test]
fn sanitizer_removes_control_sequences_without_losing_visible_content() {
    let hostile = "Overloaded\x1b]52;c;Zm9v\x07\x1b]0;pwned\x1b\\\x1b]8;;https://evil.example\x07link\x1b]8;;\x07\x1b[31mred\x1b[0m\u{9b}2Jcleartail\0\x08\x7f";
    assert_eq!(sanitize_tui_error_message(hostile), "Overloadedlinkredcleartail");
}

#[test]
fn sanitizer_normalizes_line_endings_and_horizontal_space() {
    assert_eq!(sanitize_tui_error_message("a\r\nb\rc  \t d\n"), "a\nb\nc d\n");
}

#[test]
fn sanitizer_strips_controls_from_error_attribution() {
    let result = format_extension_error_headline("/ext\x1b]52;c;Zm9v\x07.ts", None, "boom");
    assert!(!result.chars().any(char::is_control));
    assert!(result.contains("/ext.ts"));
}

#[test]
fn sanitizer_preserves_malformed_csi_text() {
    assert_eq!(sanitize_tui_error_message("a\x1b[123\nb"), "a[123\nb");
}

#[test]
fn sanitizer_consumes_unterminated_osc() {
    assert_eq!(sanitize_tui_error_message("a\x1b]52;c;payload"), "a");
}

#[test]
fn startup_resolves_only_fd() {
    let mut requested = Vec::new();
    let paths = resolve_startup_tool_paths(|tool| { requested.push(tool.to_owned()); Some("/bin/fd".into()) });
    assert_eq!(requested, ["fd"]);
    assert_eq!(paths.fd_path.as_deref(), Some("/bin/fd"));
}

#[test]
fn startup_does_not_download_missing_tools() {
    assert_eq!(resolve_startup_tool_paths(|_| None).fd_path, None);
}

#[test]
fn cancelled_login_is_status_while_provider_failure_is_error() {
    assert_eq!(describe_login_failure(None, "test", LoginMethod::Oauth).level, NoticeLevel::Status);
    let failure = LoginFailure::Failed { message: "network".into() };
    assert_eq!(describe_login_failure(Some(&failure), "test", LoginMethod::Oauth).level, NoticeLevel::Error);
}

#[test]
fn abort_and_explicit_cancellation_do_not_become_errors() {
    assert!(is_login_cancellation(Some(&LoginFailure::Abort { message: "aborted".into() })));
    assert!(is_login_cancellation(Some(&LoginFailure::Failed { message: LOGIN_CANCELLED_NOTICE.into() })));
}

fn info(source: &str, scope: SourceScope) -> SourceInfo {
    SourceInfo { path: "/resource".into(), source: source.into(), scope, origin: SourceOrigin::TopLevel, base_dir: None }
}

#[test]
fn system_scope_takes_precedence_over_cli() {
    assert_eq!(get_resource_scope_group(Some(&info("cli", SourceScope::System))), ResourceScopeGroup::System);
    assert_eq!(get_resource_scope_group(Some(&info("cli", SourceScope::User))), ResourceScopeGroup::Path);
    assert_eq!(get_resource_scope_group(None), ResourceScopeGroup::Project);
}

#[test]
fn only_system_resources_are_system() {
    assert!(is_system_resource(Some(&info("local", SourceScope::System))));
    assert!(!is_system_resource(Some(&info("local", SourceScope::User))));
    assert!(!is_system_resource(None));
}

#[test]
fn resource_groups_preserve_package_ownership_and_scope_order() {
    let items: Vec<_> = [("cli", SourceScope::System), ("cli", SourceScope::Temporary), ("local", SourceScope::User), ("npm:x", SourceScope::Project)]
        .into_iter().map(|(source, scope)| ScopedResource { path: source.into(), source_info: Some(info(source, scope)) }).collect();
    let groups = build_resource_scope_groups(&items);
    assert_eq!(groups.iter().map(|group| group.scope).collect::<Vec<_>>(), [ResourceScopeGroup::Project, ResourceScopeGroup::User, ResourceScopeGroup::Path, ResourceScopeGroup::System]);
    assert_eq!(groups[0].packages[0].0, "npm:x");
    assert_eq!(groups[3].paths[0].path, "cli");
}

#[test]
fn autocomplete_tags_cover_every_scope() {
    assert_eq!([SourceScope::User, SourceScope::Project, SourceScope::Temporary, SourceScope::System].map(get_scope_autocomplete_tag), ["u", "p", "t", "s"]);
}

#[test]
fn diagnostic_source_labels_distinguish_system_and_temporary() {
    assert_eq!(get_display_source_info(Some(&info("cli", SourceScope::System))).label, "system");
    assert_eq!(get_display_source_info(Some(&info("cli", SourceScope::Temporary))).scope_label, Some("temp"));
}

#[test]
fn configured_tmux_produces_no_warning() {
    assert_eq!(build_tmux_setup_warning(&TmuxSetupCheck { extended_keys: Some("on"), images_enabled: true, ..Default::default() }), None);
}

#[test]
fn extended_keys_always_is_accepted() {
    assert_eq!(build_tmux_setup_warning(&TmuxSetupCheck { extended_keys: Some("always"), ..Default::default() }), None);
}

#[test]
fn missing_tmux_settings_are_combined_and_aligned() {
    let warning = build_tmux_setup_warning(&TmuxSetupCheck { extended_keys_format: Some("xterm"), outer_kitty_capable: true, ..Default::default() }).expect("warning");
    let lines: Vec<_> = warning.lines().filter(|line| line.starts_with("  set -g")).collect();
    assert_eq!(lines.len(), 4);
    assert!(lines.iter().all(|line| line.find('#') == lines[0].find('#')));
}

#[test]
fn unknown_key_format_does_not_recommend_a_format_change() {
    let warning = build_tmux_setup_warning(&TmuxSetupCheck::default()).expect("warning");
    assert_eq!(warning.lines().filter(|line| line.starts_with("  set -g")).count(), 1);
}

#[test]
fn working_images_need_no_passthrough_recommendations() {
    let warning = build_tmux_setup_warning(&TmuxSetupCheck { images_enabled: true, outer_kitty_capable: true, ..Default::default() }).expect("warning");
    assert_eq!(warning.lines().filter(|line| line.starts_with("  set -g")).count(), 1);
}

#[test]
fn unsupported_outer_terminal_needs_no_passthrough() {
    let warning = build_tmux_setup_warning(&TmuxSetupCheck::default()).expect("warning");
    assert_eq!(warning.lines().filter(|line| line.starts_with("  set -g")).count(), 1);
}

#[test]
fn keyboard_ready_tmux_still_requires_image_configuration() {
    let warning = build_tmux_setup_warning(&TmuxSetupCheck { extended_keys: Some("on"), outer_kitty_capable: true, ..Default::default() }).expect("warning");
    assert_eq!(warning.lines().filter(|line| line.starts_with("  set -g")).count(), 2);
}

#[test]
fn old_tmux_recommends_upgrade_instead_of_unsupported_settings() {
    let warning = build_tmux_setup_warning(&TmuxSetupCheck { extended_keys: Some("on"), outer_kitty_capable: true, version: Some("3.2a"), ..Default::default() }).expect("warning");
    assert_eq!(warning.lines().filter(|line| line.starts_with("  upgrade tmux")).count(), 1);
}

#[test]
fn keybindings_seed_never_overwrites_existing_config() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("keybindings.json");
    std::fs::write(&path, "existing").expect("write");
    let manager = KeybindingsManager::new(Default::default(), None);
    assert!(!seed_keybindings_file(&path, &manager).expect("seed"));
    assert_eq!(std::fs::read_to_string(path).expect("read"), "existing");
}

#[test]
fn keybindings_seed_and_reload_preserve_user_override() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("keybindings.json");
    let mut manager = KeybindingsManager::create(Some(dir.path().to_str().expect("path")));
    assert!(seed_keybindings_file(&path, &manager).expect("seed"));
    std::fs::write(&path, r#"{"app.interrupt":"ctrl+j"}"#).expect("edit");
    assert_eq!(apply_keybindings_file_edit(&path, &mut manager), KeybindingsEditResult::Reloaded);
    assert_eq!(manager.get_effective_config()["app.interrupt"], Some(maho_tui::keybindings::Keys::One("ctrl+j".into())));
}

#[test]
fn invalid_keybindings_edit_keeps_current_bindings() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("keybindings.json");
    let mut manager = KeybindingsManager::create(Some(dir.path().to_str().expect("path")));
    let before = manager.get_effective_config();
    std::fs::write(&path, "{").expect("write");
    assert!(matches!(apply_keybindings_file_edit(&path, &mut manager), KeybindingsEditResult::Invalid { .. }));
    assert_eq!(manager.get_effective_config(), before);
}
