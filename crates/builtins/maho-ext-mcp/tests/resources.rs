use maho_ext_mcp::resources::*;
use serde_json::json;
#[test]
fn binary_resource_preserves_placeholder_size() {
    assert_eq!(flatten_resource_contents(&[json!({"blob":"QUFBQQ==","mimeType":"image/png","uri":"fake://bin"})]),"[binary resource fake://bin (image/png), ~6 bytes]");
}
#[tokio::test]
async fn unknown_mentions_remain_unchanged_with_notice() {
    let result=expand_mcp_resource_mentions("See @mcp:nope/some://uri now",&[]).await.unwrap();assert!(!result.changed);assert_eq!(result.text,"See @mcp:nope/some://uri now");assert_eq!(result.notices.len(),1);
}
#[tokio::test]
async fn malformed_mentions_pass_through() {
    let result=expand_mcp_resource_mentions("See @mcp:missing-slash now",&[]).await.unwrap();assert!(!result.changed);assert!(result.notices.is_empty());
}
