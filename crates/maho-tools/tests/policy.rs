use std::sync::Arc;
use maho_tools::{definition::*, filesystem_policy::*, write::*};
use serde_json::json;

#[tokio::test]
async fn write_denial_preserves_reason_and_has_no_side_effects() {
    let temp = tempfile::tempdir().unwrap();
    let allowed = temp.path().join("allowed");
    let checker: FilesystemPolicyChecker = Arc::new(move |request| {
        let allow = request.canonical_path.starts_with(&allowed);
        Box::pin(async move { Ok(if allow { FilesystemPolicyDecision::Allow } else { FilesystemPolicyDecision::Deny { reason:"Write outside filesystem-policy roots is refused".into() } }) })
    });
    let tool = create_write_tool_definition(temp.path().into(),WriteToolOptions { filesystem_policy:Some(checker),..Default::default() });
    let error = (tool.execute)(ToolCall { id:"policy-qa",params:json!({"path":"outside/new.txt","content":"not written"}),signal:AbortSignal::default(),on_update:None,context:None }).await.unwrap_err();
    assert_eq!(error.to_string(),"Write outside filesystem-policy roots is refused");
    assert!(!temp.path().join("outside").exists());
    println!("{error}");
}

#[tokio::test]
async fn first_denial_short_circuits_later_policies() {
    let deny = FilesystemPolicy { check:Arc::new(|_| Box::pin(async { Ok(FilesystemPolicyDecision::Deny { reason:"first".into() }) })),denied_roots:None };
    let later = FilesystemPolicy { check:Arc::new(|_| panic!("later policy ran after denial")),denied_roots:None };
    let checker = compose_filesystem_policies(vec![deny,later]).unwrap();
    assert_eq!(checker(FilesystemPolicyRequest { operation:FilesystemOperation::Write,canonical_path:"/x".into(),tool_name:"write".into() }).await.unwrap(),FilesystemPolicyDecision::Deny { reason:"first".into() });
}

#[cfg(unix)]
#[tokio::test]
async fn canonicalizes_missing_descendants_through_symlink() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("real")).unwrap();
    std::os::unix::fs::symlink(temp.path().join("real"),temp.path().join("alias")).unwrap();
    assert_eq!(canonicalize_filesystem_path(&temp.path().join("alias/new/file")).await.unwrap(),temp.path().join("real/new/file"));
}
