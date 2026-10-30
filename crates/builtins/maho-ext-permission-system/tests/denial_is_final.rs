//! Port of test/permission/denial-is-final.test.ts and prompt-external-directory.test.ts.
use maho_ext_permission_system::{
    events::PermissionEventEmitter, non_interactive::handle_no_ui,
    prompt::format_request_for_display, types::*,
};

fn request(permission: &str, patterns: &[&str]) -> Request {
    Request::new(
        "request-1".into(),
        "session-1".into(),
        permission.into(),
        patterns.iter().map(|p| (*p).into()).collect(),
        vec!["git push *".into()],
        Default::default(),
    )
}

#[test]
fn every_refusal_tells_the_model_not_to_reach_the_refused_result_another_way() {
    let no_ui = handle_no_ui(
        &request("bash", &["git push"]),
        false,
        &PermissionEventEmitter::default(),
    )
    .expect("emit");
    for message in [
        PermissionError::Rejected.to_string(),
        PermissionError::Corrected("not now".into()).to_string(),
        PermissionError::Denied(vec!["git push".into()]).to_string(),
        no_ui.message.unwrap_or_default(),
    ] {
        assert!(message.contains(NO_OTHER_ROUTE), "{message}");
    }
}

#[test]
fn refusals_keep_the_first_sentence_clients_match_as_a_prefix() {
    assert!(
        PermissionError::Rejected
            .to_string()
            .starts_with("The user rejected permission to use this specific tool call.")
    );
    assert!(PermissionError::Denied(vec!["x".into()]).to_string().starts_with("The user has specified a rule which prevents you from using this specific tool call."));
    assert!(PermissionError::Corrected("f".into()).to_string().starts_with("The user rejected permission to use this specific tool call with the following feedback: f"));
}

#[test]
fn headless_rejection_names_the_flag_after_the_final_denial() {
    let outcome = handle_no_ui(
        &request("bash", &["rm -rf /"]),
        false,
        &PermissionEventEmitter::default(),
    )
    .expect("emit");
    assert_eq!(
        outcome.message.as_deref(),
        Some(format!("Permission required for bash (rm -rf /), and there is no UI to ask, so the call did not run. {DENIAL_IS_FINAL} The user can allow it with --permission bash=allow.").as_str())
    );
}

fn directories(request: &Request) -> Vec<String> {
    format_request_for_display(request)
        .lines()
        .filter_map(|line| line.strip_prefix("Directory: "))
        .map(str::to_owned)
        .collect()
}

/// senpi#4311/#4316: the prompt names the resolved target, ignoring caller display metadata, and
/// a literal `*` in a filename is not cut as glob syntax.
#[test]
fn external_directory_prompt_names_the_resolved_target() {
    for target in ["/tmp/outside.txt", "/tmp/legal*filename.txt"] {
        let mut spoofed = request("external_directory", &[target]);
        spoofed
            .metadata
            .insert("filepath".into(), format!("@{target}").into());
        spoofed
            .metadata
            .insert("parentDir".into(), "/trusted-looking-project".into());
        assert_eq!(directories(&spoofed), vec![target]);
        spoofed.metadata.remove("parentDir");
        assert_eq!(directories(&spoofed), vec![target]);
    }
}

#[test]
fn external_directory_prompt_lists_the_requested_spellings() {
    let mut aliased = request("external_directory", &["/real/outside/file.txt"]);
    aliased.original_paths = Some(vec!["~/alias/file.txt".into()]);
    let shown = format_request_for_display(&aliased);
    assert!(shown.contains("Directory: /real/outside/file.txt"));
    assert!(shown.contains("Requested paths:\n  - ~/alias/file.txt"));
}
