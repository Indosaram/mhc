use maho_cli::cli::{auth_command::{parse_auth_command, AuthCommandKind}, experimental::command::{parse_options, string_option, flag_option}};
fn args(values: &[&str]) -> Vec<String> { values.iter().map(|v| (*v).to_owned()).collect() }
#[test] fn syntax_html_uses_innermost_scope_and_prefix_fallback() {
    use maho_cli::utils::syntax_highlight::*;
    let mut theme = HighlightTheme::new();
    theme.insert("string".to_owned(), Box::new(|text| format!("[{text}]")));
    theme.insert("default".to_owned(), Box::new(|text| format!("({text})")));
    assert_eq!(render_highlighted_html("plain<span class='hljs-string.special'>a&amp;<span>b</span></span>", &theme), "(plain)[a&][b]");
    assert_eq!(render_highlighted_html("<spanish>x</span>", &HighlightTheme::new()), "<spanish>x");
}
#[test] fn clipboard_osc52_payload_is_bounded() {
    assert_eq!(maho_cli::utils::clipboard::osc52_sequence("abc").as_deref(), Some("\x1b]52;c;YWJj\x07"));
    assert!(maho_cli::utils::clipboard::osc52_sequence(&"x".repeat(75_001)).is_none());
}
#[tokio::test] async fn clipboard_command_reads_and_enforces_maximum_without_desktop_access() {
    use maho_cli::utils::clipboard_command::*;
    assert_eq!(run_clipboard_command("printf", &["abc"], Default::default()).await, Some(b"abc".to_vec()));
    assert!(run_clipboard_command("printf", &["abcd"], ClipboardCommandOptions { max_buffer_bytes: Some(3), ..Default::default() }).await.is_none());
}
#[test] fn auth_help_recognizes_help_before_kind_validation() {
    use maho_cli::cli::auth_command::*;
    assert!(is_auth_command_help(&args(&["auth"])));
    assert!(is_auth_command_help(&args(&["auth", "invalid", "-h"])));
    assert!(!is_auth_command_help(&args(&["auth", "check"])));
}
#[test] fn help_cache_invalidates_when_discovery_inputs_appear() {
    use maho_cli::cli::help_flags_cache::*;
    let home = tempfile::tempdir().unwrap();
    let scope = HelpFlagsScope { cwd: home.path().join("project"), agent_dir: home.path().join("agent"), cli_extension_paths: Vec::new(), no_extensions: false, project_trusted: false };
    let flags = vec![serde_json::json!({"name":"native","type":"boolean"})];
    write_help_flags_cache(&scope, &flags, &[], "1.0");
    assert_eq!(read_help_flags_cache(&scope, "1.0"), Some(flags));
    assert!(read_help_flags_cache(&scope, "2.0").is_none());
    std::fs::write(scope.agent_dir.join("settings.json"), "{}").unwrap();
    assert!(read_help_flags_cache(&scope, "1.0").is_none());
}
#[test] fn ansi_strips_color_and_osc_sequences() {
    assert_eq!(maho_cli::utils::ansi::strip_ansi("\x1b[31mred\x1b[0m\x1b]52;c;data\x07"), "red");
}
#[test] fn exif_absent_preserves_pixels() {
    use maho_cli::utils::exif_orientation::*;
    let pixels = [1, 2, 3, 255, 4, 5, 6, 255];
    assert_eq!(get_exif_orientation(b"not an image"), 1);
    assert_eq!(apply_exif_orientation(&pixels, 2, 1, b""), (pixels.to_vec(), 2, 1));
}
#[test] fn auth_options_are_removed_from_provider_args() { let command = parse_auth_command(&args(&["auth", "check", "--json", "--no-refresh", "--provider", "xai"])).unwrap().unwrap(); assert!(command.json && command.no_refresh); assert_eq!(command.args, args(&["--provider", "xai"])); }
#[test] fn bearer_minimum_expiry_is_in_milliseconds() { let command = parse_auth_command(&args(&["auth", "print-bearer-token", "--min-expiry", "30m"])).unwrap().unwrap(); assert!(command.kind == AuthCommandKind::BearerToken); assert_eq!(command.min_expiry_ms, Some(1_800_000)); }
#[test] fn auth_rejects_flags_for_wrong_kind() { assert!(parse_auth_command(&args(&["auth", "print-api-key", "--json"])).is_err()); assert!(parse_auth_command(&args(&["auth", "check", "--min-expiry", "1h"])).is_err()); }
#[test] fn option_parser_stops_at_first_unknown_and_preserves_separator() { let result = parse_options(&args(&["--model=a", "--", "--model=b"]), &[string_option("--model", false)]); assert_eq!(result.value("--model"), Some("a")); assert_eq!(result.remaining_args, args(&["--", "--model=b"])); assert!(result.errors.is_empty()); }
#[test] fn option_parser_aggregates_duplicate_and_missing_value_errors() { let result = parse_options(&args(&["--model=a", "--model=b", "--flag=yes", "--model"]), &[string_option("--model", false), flag_option("--flag")]); assert_eq!(result.errors.len(), 3); assert_eq!(result.value("--model"), Some("a")); }
#[test] fn option_parser_preserves_repeatable_order() { let result = parse_options(&args(&["-e=a", "-e", "b"]), &[string_option("-e", true)]); assert_eq!(result.values["-e"], args(&["a", "b"])); }
#[test] fn html_decodes_named_and_numeric_prefix_entities() { use maho_cli::utils::html::*; assert_eq!(decode_html_entity("amp").as_deref(), Some("&")); assert_eq!(decode_html_entity("#x1f600").as_deref(), Some("😀")); assert_eq!(decode_html_entity("#65tail").as_deref(), Some("A")); assert!(decode_html_entity("#-1").is_none()); let entity = decode_html_entity_at("😀&amp;", 2).unwrap(); assert_eq!(entity.length, 5); assert_eq!(entity.text, "&"); }
#[tokio::test] async fn sleep_cancels_on_signal() { let controller = maho_ai::utils::abort::AbortController::new(); controller.abort(None); assert!(maho_cli::utils::sleep::sleep(60_000, Some(&controller.signal())).await.is_err()); }
