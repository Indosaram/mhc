use maho_ext_mcp::auth::{
    challenge::{AuthChallenge, merge_scopes, parse_www_authenticate, step_up_required},
    oauth_provider::McpOAuthProvider,
    token_store::McpTokenStore,
};

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn parses_quoted_scopes_and_multiple_params() {
    let header = r#"Bearer realm="mcp, test", error="insufficient_scope", scope="read  write, x" , error_description="needs \"more\"", resource_metadata=https://rs.test/meta"#;
    let challenge = parse_www_authenticate(header).unwrap();
    assert_eq!(
        challenge,
        AuthChallenge {
            scopes: strings(&["read", "write,", "x"]),
            error: Some("insufficient_scope".into()),
            error_description: Some("needs \"more\"".into()),
            resource_metadata: Some("https://rs.test/meta".into()),
        }
    );
    assert_eq!(parse_www_authenticate("bEaReR scope=a").unwrap().scopes, strings(&["a"]));
}

#[test]
fn non_bearer_or_parameterless_challenges_yield_nothing() {
    assert_eq!(parse_www_authenticate(r#"Basic realm="x""#), None);
    assert_eq!(parse_www_authenticate("Bearer"), None);
    assert_eq!(parse_www_authenticate("Bearer   "), None);
    assert_eq!(parse_www_authenticate(""), None);
}

#[test]
fn step_up_only_for_403_with_insufficient_scope_or_scopes() {
    let header = r#"Bearer error="insufficient_scope", scope="a b""#;
    assert_eq!(step_up_required(403, Some(header)).unwrap().scopes, strings(&["a", "b"]));
    assert!(step_up_required(401, Some(header)).is_none());
    assert!(step_up_required(403, None).is_none());
    assert!(step_up_required(403, Some(r#"Bearer error="invalid_token""#)).is_none());
    assert!(step_up_required(403, Some(r#"Bearer scope="a""#)).is_some());
    assert!(step_up_required(403, Some(r#"Bearer error="insufficient_scope""#)).is_some());
    assert!(step_up_required(403, Some(r#"Basic scope="a""#)).is_none());
}

#[test]
fn merge_keeps_first_seen_order_without_duplicates() {
    assert_eq!(merge_scopes(&strings(&["a", "b"]), &strings(&["b", "c", "a", "c"])), strings(&["a", "b", "c"]));
    assert!(merge_scopes(&[], &[]).is_empty());
}

#[test]
fn provider_effective_scopes_are_configured_then_escalated() {
    let root = tempfile::tempdir().unwrap();
    let mut provider = McpOAuthProvider::new(McpTokenStore::new(root.path(), "s", "https://x.test"));
    assert_eq!(provider.effective_scopes(), None);
    provider.escalate_scopes(&strings(&["w"]));
    assert_eq!(provider.effective_scopes(), Some(strings(&["w"])));
    provider.scopes = Some(strings(&["r", "w"]));
    provider.escalate_scopes(&strings(&["x", "r"]));
    assert_eq!(provider.effective_scopes(), Some(strings(&["r", "w", "x"])));
    // Registration metadata is unaffected by escalation.
    assert_eq!(provider.client_metadata()["scope"], "r w");
}
