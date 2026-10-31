use maho_cli::cli::args::*;
#[test]
fn parses_help_flag() {
    let value = parse(&["--help"]);
    assert!(value.help);
}
#[test]
fn parses_h_flag() {
    let value = parse(&["-h"]);
    assert!(value.help);
}
#[test]
fn parses_version_flag() {
    let value = parse(&["--version"]);
    assert!(value.version);
}
#[test]
fn parses_v_flag() {
    let value = parse(&["-v"]);
    assert!(value.version);
}
#[test]
fn parses_print_flag() {
    let value = parse(&["--print"]);
    assert!(value.print);
}
#[test]
fn parses_p_flag() {
    let value = parse(&["-p"]);
    assert!(value.print);
}
#[test]
fn parses_continue_flag() {
    let value = parse(&["--continue"]);
    assert!(value.continue_session);
}
#[test]
fn parses_c_flag() {
    let value = parse(&["-c"]);
    assert!(value.continue_session);
}
#[test]
fn parses_resume_flag() {
    let value = parse(&["--resume"]);
    assert!(value.resume);
}
#[test]
fn parses_r_flag() {
    let value = parse(&["-r"]);
    assert!(value.resume);
}
#[test]
fn parses_no_session_flag() {
    let value = parse(&["--no-session"]);
    assert!(value.no_session);
}
#[test]
fn parses_no_tools_flag() {
    let value = parse(&["--no-tools"]);
    assert!(value.no_tools);
}
#[test]
fn parses_nt_flag() {
    let value = parse(&["-nt"]);
    assert!(value.no_tools);
}
#[test]
fn parses_no_builtin_tools_flag() {
    let value = parse(&["--no-builtin-tools"]);
    assert!(value.no_builtin_tools);
}
#[test]
fn parses_nbt_flag() {
    let value = parse(&["-nbt"]);
    assert!(value.no_builtin_tools);
}
#[test]
fn parses_no_extensions_flag() {
    let value = parse(&["--no-extensions"]);
    assert!(value.no_extensions);
}
#[test]
fn parses_ne_flag() {
    let value = parse(&["-ne"]);
    assert!(value.no_extensions);
}
#[test]
fn parses_no_skills_flag() {
    let value = parse(&["--no-skills"]);
    assert!(value.no_skills);
}
#[test]
fn parses_ns_flag() {
    let value = parse(&["-ns"]);
    assert!(value.no_skills);
}
#[test]
fn parses_no_prompt_templates_flag() {
    let value = parse(&["--no-prompt-templates"]);
    assert!(value.no_prompt_templates);
}
#[test]
fn parses_np_flag() {
    let value = parse(&["-np"]);
    assert!(value.no_prompt_templates);
}
#[test]
fn parses_no_themes_flag() {
    let value = parse(&["--no-themes"]);
    assert!(value.no_themes);
}
#[test]
fn parses_no_context_files_flag() {
    let value = parse(&["--no-context-files"]);
    assert!(value.no_context_files);
}
#[test]
fn parses_nc_flag() {
    let value = parse(&["-nc"]);
    assert!(value.no_context_files);
}
#[test]
fn parses_verbose_flag() {
    let value = parse(&["--verbose"]);
    assert!(value.verbose);
}
#[test]
fn parses_offline_flag() {
    let value = parse(&["--offline"]);
    assert!(value.offline);
}
#[test]
fn parses_multi_session_flag() {
    let value = parse(&["--multi-session"]);
    assert!(value.multi_session);
}
#[test]
fn parses_auto_title_sessions_flag() {
    let value = parse(&["--auto-title-sessions"]);
    assert!(value.auto_title_sessions);
}
#[test]
fn parses_list_tips_flag() {
    let value = parse(&["--list-tips"]);
    assert!(value.list_tips);
}
#[test]
fn parses_provider_value() {
    let value = parse(&["--provider", "sample"]);
    assert_eq!(value.provider.as_deref(), Some("sample"));
}
#[test]
fn parses_model_value() {
    let value = parse(&["--model", "sample"]);
    assert_eq!(value.model.as_deref(), Some("sample"));
}
#[test]
fn parses_api_key_value() {
    let value = parse(&["--api-key", "sample"]);
    assert_eq!(value.api_key.as_deref(), Some("sample"));
}
#[test]
fn parses_system_prompt_value() {
    let value = parse(&["--system-prompt", "sample"]);
    assert_eq!(value.system_prompt.as_deref(), Some("sample"));
}
#[test]
fn parses_session_value() {
    let value = parse(&["--session", "sample"]);
    assert_eq!(value.session.as_deref(), Some("sample"));
}
#[test]
fn parses_session_id_value() {
    let value = parse(&["--session-id", "sample"]);
    assert_eq!(value.session_id.as_deref(), Some("sample"));
}
#[test]
fn parses_fork_value() {
    let value = parse(&["--fork", "sample"]);
    assert_eq!(value.fork.as_deref(), Some("sample"));
}
#[test]
fn parses_rebind_value() {
    let value = parse(&["--rebind", "sample"]);
    assert_eq!(value.rebind.as_deref(), Some("sample"));
}
#[test]
fn parses_session_dir_value() {
    let value = parse(&["--session-dir", "sample"]);
    assert_eq!(value.session_dir.as_deref(), Some("sample"));
}
#[test]
fn parses_export_value() {
    let value = parse(&["--export", "sample"]);
    assert_eq!(value.export.as_deref(), Some("sample"));
}
#[test]
fn parses_print_prompt_sizes_flag() {
    assert!(parse(&["--print-prompt-sizes"]).print_prompt_sizes);
}
#[test]
fn parses_use_theme_value() {
    let value = parse(&["--use-theme", "sample"]);
    assert_eq!(value.use_theme.as_deref(), Some("sample"));
}
#[test]
fn parses_name_value() {
    let value = parse(&["--name", "sample"]);
    assert_eq!(value.name.as_deref(), Some("sample"));
}
#[test]
fn parses_n_value() {
    let value = parse(&["-n", "sample"]);
    assert_eq!(value.name.as_deref(), Some("sample"));
}
#[test]
fn consumes_empty_search_when_list_models_is_followed_by_empty_argument() {
    let value = parse(&["--list-models", "", "hello"]);
    assert_eq!(value.messages, ["hello"]);
}
#[test]
fn parses_valid_output_modes() {
    assert!(parse(&["--mode", "text"]).mode == Some(Mode::Text));
    assert!(parse(&["--mode", "json"]).mode == Some(Mode::Json));
    assert!(parse(&["--mode", "rpc"]).mode == Some(Mode::Rpc));
}
#[test]
fn accepts_all_thinking_levels() {
    for level in ["off", "minimal", "low", "medium", "high", "xhigh", "max"] {
        assert_eq!(
            parse(&["--thinking", level]).thinking.as_deref(),
            Some(level)
        );
    }
}
#[test]
fn approval_override_uses_last_flag() {
    assert_eq!(
        parse(&["--approve", "--no-approve"]).project_trust_override,
        Some(false)
    );
    assert_eq!(parse(&["-na", "-a"]).project_trust_override, Some(true));
}
#[test]
fn missing_name_reports_error() {
    assert!(parse(&["--name"]).diagnostics[0].error);
}
#[test]
fn unknown_short_options_report_error() {
    assert!(parse(&["-invalid"]).diagnostics[0].error);
}
#[test]
fn malformed_runtime_does_not_consume_next_long_flag() {
    let value = parse(&["--session-runtime", "--help"]);
    assert!(value.help);
    assert!(value.diagnostics[0].error);
}
#[test]
fn missing_rpc_listener_preserves_following_flag() {
    let value = parse(&["--mode", "rpc", "--listen", "--help"]);
    assert!(value.help);
    assert!(value.diagnostics[0].error);
}
fn parse(values: &[&str]) -> Args {
    parse_args(
        &values.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
        false,
    )
    .expect("parse")
}
#[test]
fn consumes_frontmatter_prompt_when_print_precedes_it() {
    let value = parse(&["-p", "---\ntitle: hello\n---\nSay hi."]);
    assert!(value.print);
    assert_eq!(value.messages.len(), 1);
    assert!(value.unknown_flags.is_empty());
}
#[test]
fn preserves_options_when_print_precedes_provider() {
    let value = parse(&["-p", "--provider", "openai", "Say hi."]);
    assert_eq!(value.provider.as_deref(), Some("openai"));
    assert_eq!(value.messages, ["Say hi."]);
}
#[test]
fn separator_preserves_dash_messages_and_file_arguments() {
    let value = parse(&["--", "-message", "@image.png"]);
    assert_eq!(value.messages, ["-message"]);
    assert_eq!(value.file_args, ["image.png"]);
}
#[test]
fn missing_theme_does_not_consume_following_flag() {
    let value = parse(&["--use-theme", "--print"]);
    assert!(value.print);
    assert!(value.use_theme.is_none());
    assert!(value.diagnostics[0].error);
}
#[test]
fn runtime_defaults_to_in_process_when_socket_is_requested() {
    let value = parse(&["--mode", "rpc", "--listen", "unix:///tmp/socket"]);
    assert!(value.multi_session);
    assert!(resolve_session_runtime(&value) == SessionRuntime::InProcess);
}
#[test]
fn runtime_override_wins_when_socket_is_requested() {
    let value = parse(&[
        "--mode",
        "rpc",
        "--listen",
        "unix:///tmp/socket",
        "--session-runtime",
        "worker",
    ]);
    assert!(resolve_session_runtime(&value) == SessionRuntime::Worker);
}
#[test]
fn listen_remains_extension_flag_when_rpc_mode_is_not_yet_selected() {
    let value = parse(&["--listen", "unix://", "--mode", "rpc"]);
    assert!(value.listen.is_none());
    assert!(value.unknown_flags.contains_key("listen"));
}
#[test]
fn unknown_values_preserve_equals_and_boolean_forms() {
    let value = parse(&["--one=value", "--two", "@file", "--three", "string"]);
    assert!(value.unknown_flags.get("one") == Some(&FlagValue::String("value".to_owned())));
    assert!(value.unknown_flags.get("two") == Some(&FlagValue::Boolean(true)));
    assert!(value.unknown_flags.get("three") == Some(&FlagValue::String("string".to_owned())));
}
#[test]
fn legacy_provider_ids_are_rejected_when_typed() {
    for id in ["openai-codex", "claude-sdk-oauth"] {
        assert!(parse_args(&["--provider".to_owned(), id.to_owned()], false).is_err());
    }
}
#[test]
fn grok_flag_requires_explicit_gate() {
    let argv = ["--grok-neo".to_owned()];
    assert!(
        parse_args(&argv, false)
            .expect("parse")
            .unknown_flags
            .contains_key("grok-neo")
    );
    assert!(parse_args(&argv, true).expect("parse").grok_neo);
}
#[test]
fn repeated_resources_and_prompts_accumulate_when_given_multiple_times() {
    let value = parse(&[
        "-e",
        "a",
        "--extension",
        "b",
        "--skill",
        "s",
        "--skill",
        "t",
        "--append-system-prompt",
        "x",
        "--append-system-prompt",
        "y",
    ]);
    assert_eq!(value.extensions, ["a", "b"]);
    assert_eq!(value.skills, ["s", "t"]);
    assert_eq!(value.append_system_prompt, ["x", "y"]);
}
#[test]
fn comma_lists_drop_empty_tools_but_keep_empty_model_patterns() {
    let value = parse(&["--tools", " read,, bash ", "--models", "a,, b"]);
    assert_eq!(
        value.tools,
        Some(vec!["read".to_owned(), "bash".to_owned()])
    );
    assert_eq!(
        value.models,
        Some(vec!["a".to_owned(), "".to_owned(), "b".to_owned()])
    );
}
#[test]
fn name_normalization_rejects_whitespace_when_validated() {
    assert_eq!(normalize_session_name("  named  "), Some("named"));
    assert_eq!(normalize_session_name("   "), None);
    assert_eq!(parse(&["--name", ""]).name.as_deref(), Some(""));
}
#[test]
fn invalid_thinking_reports_warning_when_value_is_unknown() {
    let value = parse(&["--thinking", "other"]);
    assert!(value.thinking.is_none());
    assert!(!value.diagnostics[0].error);
}
#[test]
fn extension_flags_section_lists_flags_with_value_hint_and_fallback_description() {
    let text = extension_flags_text(&[
        serde_json::json!({"name":"plan","type":"boolean","description":"Plan mode"}),
        serde_json::json!({"name":"depth","type":"string","extensionPath":"<user:x>"}),
    ]);
    assert_eq!(
        text,
        format!(
            "\n{:<30}Plan mode\n{:<30}Registered by <user:x>\n",
            "  --plan", "  --depth <value>"
        )
    );
    assert!(extension_flags_text(&[]).is_empty());
}
// senpi v2026.10.27-2 cli-spawned-by-args.test.ts (oh-my-openagent#10782).
#[test]
fn spawned_by_parses_one_json_object_without_consuming_the_prompt() {
    let value = parse(&[
        "--spawned-by",
        r#"{"parentSessionId":"parent","rootSessionId":"root","taskId":"task"}"#,
        "hello",
    ]);
    let spawned_by = value.spawned_by.expect("ancestry");
    assert_eq!(spawned_by.parent_session_id, "parent");
    assert_eq!(spawned_by.root_session_id, "root");
    assert_eq!(spawned_by.task_id, "task");
    assert_eq!(value.messages, vec!["hello".to_owned()]);
    assert!(value.diagnostics.is_empty());
    assert!(value.unknown_flags.is_empty());
}
#[test]
fn spawned_by_rejects_malformed_ancestry() {
    let mut cases: Vec<String> = ["{", "null", "[]", "true", r#""parent:root:task""#, "{}"]
        .map(str::to_owned)
        .to_vec();
    for key in ["parentSessionId", "rootSessionId", "taskId"] {
        for bad in [
            None,
            Some(serde_json::json!("")),
            Some(serde_json::json!(1)),
            Some(serde_json::Value::Null),
        ] {
            let mut ancestry = serde_json::json!({"parentSessionId":"parent","rootSessionId":"root","taskId":"task"});
            match bad {
                None => {
                    ancestry.as_object_mut().expect("object").remove(key);
                }
                Some(value) => ancestry[key] = value,
            }
            cases.push(ancestry.to_string());
        }
    }
    for case in cases {
        let value = parse(&["--spawned-by", case.as_str()]);
        assert_eq!(value.diagnostics.len(), 1, "{case}");
        assert!(value.diagnostics[0].error, "{case}");
        assert!(value.spawned_by.is_none(), "{case}");
    }
}
#[test]
fn spawned_by_rejects_a_missing_value() {
    let value = parse(&["--spawned-by"]);
    assert_eq!(value.diagnostics.len(), 1);
    assert!(value.diagnostics[0].error);
    let value = parse(&["--spawned-by", "--offline"]);
    assert_eq!(value.diagnostics.len(), 1);
    assert!(value.offline);
}
// senpi v2026.10.27-2 host-cli-presenter: only descriptor 4 is a presenter bootstrap.
#[test]
fn mcp_presenter_flags_parse_and_reject_other_descriptors() {
    let value = parse(&["--mcp-presenter-fd", "4", "--mcp-presenter-enforcement"]);
    assert_eq!(value.presenter_bootstrap_fd, Some(4));
    assert!(value.mcp_presenter_enforcement);
    assert!(value.diagnostics.is_empty());
    let value = parse(&["--mcp-presenter-fd", "3"]);
    assert_eq!(value.presenter_bootstrap_fd, None);
    assert!(value.diagnostics[0].error);
}
// senpi v2026.10.27-2 main.ts:529: ancestry is ignored, with a warning, unless the session is fresh.
#[test]
fn spawned_by_is_ignored_with_a_warning_when_resuming_or_forking() {
    let ancestry = r#"{"parentSessionId":"parent","rootSessionId":"root","taskId":"task"}"#;
    assert_eq!(
        spawned_by_ignored_warning(&parse(&["--spawned-by", ancestry])),
        None
    );
    for reuse in [
        &["--resume"][..],
        &["--continue"][..],
        &["--session", "s.jsonl"][..],
        &["--fork", "s.jsonl"][..],
    ] {
        let mut values = vec!["--spawned-by", ancestry];
        values.extend_from_slice(reuse);
        assert_eq!(
            spawned_by_ignored_warning(&parse(&values)),
            Some("--spawned-by is ignored when resuming or forking a session."),
            "{reuse:?}"
        );
    }
    assert_eq!(spawned_by_ignored_warning(&parse(&["--resume"])), None);
}
// senpi v2026.10.27-2: the TUI has no presenter authority yet, so enforcement is refused, not dropped.
#[test]
fn tui_refuses_mcp_presenter_enforcement() {
    assert_eq!(
        tui_presenter_enforcement_refusal(&parse(&["--mcp-presenter-enforcement"])),
        Some("--mcp-presenter-enforcement is not supported by this build's interactive mode yet")
    );
    assert_eq!(tui_presenter_enforcement_refusal(&parse(&[])), None);
}
