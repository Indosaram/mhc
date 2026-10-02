use maho_ext_mcp::auth::{oauth::*,oauth_refresh::*,token_store::McpStoredAuth,oauth_errors::OAuthFailureKind};
use serde_json::json;
#[test]
fn expiry_leeway_includes_exact_boundary() {
    let mut record=McpStoredAuth {access_token:Some("fixture".into()),expires_at:Some(300000.0),..Default::default()};assert!(is_token_stale(Some(&record),0.0));record.expires_at=Some(300001.0);assert!(!is_token_stale(Some(&record),0.0));record.expires_at=None;assert!(!is_token_stale(Some(&record),0.0));record.access_token=None;assert!(is_token_stale(Some(&record),0.0));
}
#[test]
fn proof_key_support_must_be_explicit() {
    assert!(assert_s256_supported(Some(&json!({"code_challenge_methods_supported":["S256"]})),"srv").is_ok());
    for metadata in [json!({}),json!({"code_challenge_methods_supported":["plain"]})] {assert_eq!(assert_s256_supported(Some(&metadata),"srv").unwrap_err().oauth_kind,OAuthFailureKind::S256Unsupported);}
}
#[test]
fn redirect_parsing_preserves_first_query_value() {
    assert_eq!(parse_redirect(" http://127.0.0.1/callback?code=a%2Bb&code=other&state=csrf ","srv").unwrap(),("a+b".into(),Some("csrf".into())));
}
#[test]
fn malformed_failed_and_empty_redirects_are_rejected() {
    for input in ["not a url","http://127.0.0.1/callback?code=","http://127.0.0.1/callback?error=access_denied&code=abc"] {assert_eq!(parse_redirect(input,"srv").unwrap_err().oauth_kind,OAuthFailureKind::NeedsAuth);}
}
