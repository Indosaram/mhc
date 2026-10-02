use maho_cli::cli::args::*;
#[test] fn parses_help_flag() { let value = parse(&["--help"]); assert!(value.help); }
#[test] fn parses_h_flag() { let value = parse(&["-h"]); assert!(value.help); }
#[test] fn parses_version_flag() { let value = parse(&["--version"]); assert!(value.version); }
#[test] fn parses_v_flag() { let value = parse(&["-v"]); assert!(value.version); }
#[test] fn parses_print_flag() { let value = parse(&["--print"]); assert!(value.print); }
#[test] fn parses_p_flag() { let value = parse(&["-p"]); assert!(value.print); }
#[test] fn parses_continue_flag() { let value = parse(&["--continue"]); assert!(value.continue_session); }
#[test] fn parses_c_flag() { let value = parse(&["-c"]); assert!(value.continue_session); }
#[test] fn parses_resume_flag() { let value = parse(&["--resume"]); assert!(value.resume); }
#[test] fn parses_r_flag() { let value = parse(&["-r"]); assert!(value.resume); }
#[test] fn parses_no_session_flag() { let value = parse(&["--no-session"]); assert!(value.no_session); }
#[test] fn parses_no_tools_flag() { let value = parse(&["--no-tools"]); assert!(value.no_tools); }
#[test] fn parses_nt_flag() { let value = parse(&["-nt"]); assert!(value.no_tools); }
#[test] fn parses_no_builtin_tools_flag() { let value = parse(&["--no-builtin-tools"]); assert!(value.no_builtin_tools); }
#[test] fn parses_nbt_flag() { let value = parse(&["-nbt"]); assert!(value.no_builtin_tools); }
#[test] fn parses_no_extensions_flag() { let value = parse(&["--no-extensions"]); assert!(value.no_extensions); }
#[test] fn parses_ne_flag() { let value = parse(&["-ne"]); assert!(value.no_extensions); }
#[test] fn parses_no_skills_flag() { let value = parse(&["--no-skills"]); assert!(value.no_skills); }
#[test] fn parses_ns_flag() { let value = parse(&["-ns"]); assert!(value.no_skills); }
#[test] fn parses_no_prompt_templates_flag() { let value = parse(&["--no-prompt-templates"]); assert!(value.no_prompt_templates); }
#[test] fn parses_np_flag() { let value = parse(&["-np"]); assert!(value.no_prompt_templates); }
#[test] fn parses_no_themes_flag() { let value = parse(&["--no-themes"]); assert!(value.no_themes); }
#[test] fn parses_no_context_files_flag() { let value = parse(&["--no-context-files"]); assert!(value.no_context_files); }
#[test] fn parses_nc_flag() { let value = parse(&["-nc"]); assert!(value.no_context_files); }
#[test] fn parses_verbose_flag() { let value = parse(&["--verbose"]); assert!(value.verbose); }
#[test] fn parses_offline_flag() { let value = parse(&["--offline"]); assert!(value.offline); }
#[test] fn parses_multi_session_flag() { let value = parse(&["--multi-session"]); assert!(value.multi_session); }
#[test] fn parses_auto_title_sessions_flag() { let value = parse(&["--auto-title-sessions"]); assert!(value.auto_title_sessions); }
#[test] fn parses_list_tips_flag() { let value = parse(&["--list-tips"]); assert!(value.list_tips); }
#[test] fn parses_provider_value() { let value = parse(&["--provider", "sample"]); assert_eq!(value.provider.as_deref(), Some("sample")); }
#[test] fn parses_model_value() { let value = parse(&["--model", "sample"]); assert_eq!(value.model.as_deref(), Some("sample")); }
#[test] fn parses_api_key_value() { let value = parse(&["--api-key", "sample"]); assert_eq!(value.api_key.as_deref(), Some("sample")); }
#[test] fn parses_system_prompt_value() { let value = parse(&["--system-prompt", "sample"]); assert_eq!(value.system_prompt.as_deref(), Some("sample")); }
#[test] fn parses_session_value() { let value = parse(&["--session", "sample"]); assert_eq!(value.session.as_deref(), Some("sample")); }
#[test] fn parses_session_id_value() { let value = parse(&["--session-id", "sample"]); assert_eq!(value.session_id.as_deref(), Some("sample")); }
#[test] fn parses_fork_value() { let value = parse(&["--fork", "sample"]); assert_eq!(value.fork.as_deref(), Some("sample")); }
#[test] fn parses_session_dir_value() { let value = parse(&["--session-dir", "sample"]); assert_eq!(value.session_dir.as_deref(), Some("sample")); }
#[test] fn parses_export_value() { let value = parse(&["--export", "sample"]); assert_eq!(value.export.as_deref(), Some("sample")); }
#[test] fn parses_use_theme_value() { let value = parse(&["--use-theme", "sample"]); assert_eq!(value.use_theme.as_deref(), Some("sample")); }
#[test] fn parses_name_value() { let value = parse(&["--name", "sample"]); assert_eq!(value.name.as_deref(), Some("sample")); }
#[test] fn parses_n_value() { let value = parse(&["-n", "sample"]); assert_eq!(value.name.as_deref(), Some("sample")); }
#[test] fn consumes_empty_search_when_list_models_is_followed_by_empty_argument() { let value = parse(&["--list-models", "", "hello"]); assert_eq!(value.messages, ["hello"]); }
#[test] fn parses_valid_output_modes() { assert!(parse(&["--mode", "text"]).mode == Some(Mode::Text)); assert!(parse(&["--mode", "json"]).mode == Some(Mode::Json)); assert!(parse(&["--mode", "rpc"]).mode == Some(Mode::Rpc)); }
#[test] fn accepts_all_thinking_levels() { for level in ["off", "minimal", "low", "medium", "high", "xhigh", "max"] { assert_eq!(parse(&["--thinking", level]).thinking.as_deref(), Some(level)); } }
#[test] fn approval_override_uses_last_flag() { assert_eq!(parse(&["--approve", "--no-approve"]).project_trust_override, Some(false)); assert_eq!(parse(&["-na", "-a"]).project_trust_override, Some(true)); }
#[test] fn missing_name_reports_error() { assert!(parse(&["--name"]).diagnostics[0].error); }
#[test] fn unknown_short_options_report_error() { assert!(parse(&["-invalid"]).diagnostics[0].error); }
#[test] fn malformed_runtime_does_not_consume_next_long_flag() { let value = parse(&["--session-runtime", "--help"]); assert!(value.help); assert!(value.diagnostics[0].error); }
#[test] fn missing_rpc_listener_preserves_following_flag() { let value = parse(&["--mode", "rpc", "--listen", "--help"]); assert!(value.help); assert!(value.diagnostics[0].error); }
fn parse(values: &[&str]) -> Args { parse_args(&values.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(), false).expect("parse") }
#[test] fn consumes_frontmatter_prompt_when_print_precedes_it() { let value = parse(&["-p", "---\ntitle: hello\n---\nSay hi."]); assert!(value.print); assert_eq!(value.messages.len(), 1); assert!(value.unknown_flags.is_empty()); }
#[test] fn preserves_options_when_print_precedes_provider() { let value = parse(&["-p", "--provider", "openai", "Say hi."]); assert_eq!(value.provider.as_deref(), Some("openai")); assert_eq!(value.messages, ["Say hi."]); }
#[test] fn separator_preserves_dash_messages_and_file_arguments() { let value = parse(&["--", "-message", "@image.png"]); assert_eq!(value.messages, ["-message"]); assert_eq!(value.file_args, ["image.png"]); }
#[test] fn missing_theme_does_not_consume_following_flag() { let value = parse(&["--use-theme", "--print"]); assert!(value.print); assert!(value.use_theme.is_none()); assert!(value.diagnostics[0].error); }
#[test] fn runtime_defaults_to_in_process_when_socket_is_requested() { let value = parse(&["--mode", "rpc", "--listen", "unix:///tmp/socket"]); assert!(value.multi_session); assert!(resolve_session_runtime(&value) == SessionRuntime::InProcess); }
#[test] fn runtime_override_wins_when_socket_is_requested() { let value = parse(&["--mode", "rpc", "--listen", "unix:///tmp/socket", "--session-runtime", "worker"]); assert!(resolve_session_runtime(&value) == SessionRuntime::Worker); }
#[test] fn listen_remains_extension_flag_when_rpc_mode_is_not_yet_selected() { let value = parse(&["--listen", "unix://", "--mode", "rpc"]); assert!(value.listen.is_none()); assert!(value.unknown_flags.contains_key("listen")); }
#[test] fn unknown_values_preserve_equals_and_boolean_forms() { let value = parse(&["--one=value", "--two", "@file", "--three", "string"]); assert!(value.unknown_flags.get("one") == Some(&FlagValue::String("value".to_owned()))); assert!(value.unknown_flags.get("two") == Some(&FlagValue::Boolean(true))); assert!(value.unknown_flags.get("three") == Some(&FlagValue::String("string".to_owned()))); }
#[test] fn legacy_provider_ids_are_rejected_when_typed() { for id in ["openai-codex", "claude-sdk-oauth"] { assert!(parse_args(&["--provider".to_owned(), id.to_owned()], false).is_err()); } }
#[test] fn grok_flag_requires_explicit_gate() { let argv = ["--grok-neo".to_owned()]; assert!(parse_args(&argv, false).expect("parse").unknown_flags.contains_key("grok-neo")); assert!(parse_args(&argv, true).expect("parse").grok_neo); }
#[test] fn repeated_resources_and_prompts_accumulate_when_given_multiple_times() { let value = parse(&["-e", "a", "--extension", "b", "--skill", "s", "--skill", "t", "--append-system-prompt", "x", "--append-system-prompt", "y"]); assert_eq!(value.extensions, ["a", "b"]); assert_eq!(value.skills, ["s", "t"]); assert_eq!(value.append_system_prompt, ["x", "y"]); }
#[test] fn comma_lists_drop_empty_tools_but_keep_empty_model_patterns() { let value = parse(&["--tools", " read,, bash ", "--models", "a,, b"]); assert_eq!(value.tools, Some(vec!["read".to_owned(), "bash".to_owned()])); assert_eq!(value.models, Some(vec!["a".to_owned(), "".to_owned(), "b".to_owned()])); }
#[test] fn name_normalization_rejects_whitespace_when_validated() { assert_eq!(normalize_session_name("  named  "), Some("named")); assert_eq!(normalize_session_name("   "), None); assert_eq!(parse(&["--name", ""]).name.as_deref(), Some("")); }
#[test] fn invalid_thinking_reports_warning_when_value_is_unknown() { let value = parse(&["--thinking", "other"]); assert!(value.thinking.is_none()); assert!(!value.diagnostics[0].error); }
