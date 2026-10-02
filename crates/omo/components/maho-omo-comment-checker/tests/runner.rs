use comment_checker_core::{HookInput,HookToolInput,RunCommentCheckerInput};
#[tokio::test]
async fn real_binary_receives_hook_and_reports_exit_two()->Result<(),Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    let root=tempfile::tempdir()?;let bin=root.path().join("checker");
    std::fs::write(&bin,"#!/bin/sh\ninput=$(cat)\ncase \"$input\" in *PostToolUse*) printf 'issue\\r\\n' >&2; exit 2;; *) exit 1;; esac\n")?;
    std::fs::set_permissions(&bin,std::fs::Permissions::from_mode(0o700))?;
    let input=RunCommentCheckerInput{hook_input:HookInput{session_id:"session".into(),tool_name:"write".into(),transcript_path:String::new(),cwd:root.path().to_string_lossy().into_owned(),hook_event_name:"PostToolUse".into(),tool_input:HookToolInput::default(),tool_response:None},binary_path:Some(bin.to_string_lossy().into_owned()),custom_prompt:None};
    let result=maho_omo_comment_checker::runner::default_run_comment_checker(&input).await?;
    assert!(result.has_comments);assert_eq!(result.message,"issue\n");Ok(())
}
