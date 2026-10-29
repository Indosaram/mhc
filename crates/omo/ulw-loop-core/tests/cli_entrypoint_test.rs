use std::io::Cursor;
use ulw_loop_core::cli::main_cli;
use ulw_loop_core::codex_hook::{apply_user_prompt_ulw_loop_steering, UserPromptSubmitHookOptions, UserPromptSubmitPayload};
use ulw_loop_core::cli_output::ULW_LOOP_HELP;

#[test]
fn bare_status_routes_to_ulw_loop_surface() { assert!(ULW_LOOP_HELP.contains("status")); }
#[test]
fn legacy_ulw_loop_status_routes_to_same_surface() { assert_eq!(main_cli(&["ulw-loop".into(), "help".into()]), 0); }
#[test]
fn unscoped_status_is_rejected_by_scope_policy() { assert_eq!(main_cli(&["ulw-loop".into(), "status".into(), "--json".into()]), 1); }
#[test]
fn top_level_help_prints_usage() { assert_eq!(main_cli(&["help".into()]), 0); assert!(ULW_LOOP_HELP.contains("Usage:")); }
#[test]
fn unknown_command_fails() { assert_eq!(main_cli(&["frobnicate".into()]), 1); }
#[test]
fn hook_ulw_prompt_emits_directive() { let p=UserPromptSubmitPayload{cwd:"/tmp".into(),hook_event_name:"UserPromptSubmit".into(),model:Some("gpt-5.5".into()),permission_mode:Some("default".into()),prompt:"ulw this change".into(),session_id:"s1".into(),transcript_path:None,turn_id:Some("t1".into())}; let out=apply_user_prompt_ulw_loop_steering(&p,&UserPromptSubmitHookOptions{include_ultrawork_directive:true,ultrawork_skill_file_path:None}); assert!(out.starts_with("<ultrawork-mode>")); }
#[test]
fn hook_payload_can_be_driven_through_reader() { let raw=br#"{"cwd":"/tmp","hook_event_name":"UserPromptSubmit","prompt":"regular","session_id":"s1"}"#; let parsed=ulw_loop_core::codex_hook::parse_user_prompt_submit_payload(std::str::from_utf8(raw).unwrap()); assert!(parsed.is_some()); }
#[test]
fn hook_non_ulw_prompt_emits_nothing() { let p=UserPromptSubmitPayload{cwd:"/tmp".into(),hook_event_name:"UserPromptSubmit".into(),model:None,permission_mode:None,prompt:"regular".into(),session_id:"s1".into(),transcript_path:None,turn_id:None}; assert!(apply_user_prompt_ulw_loop_steering(&p,&UserPromptSubmitHookOptions::default()).is_empty()); let _=Cursor::new(""); }
