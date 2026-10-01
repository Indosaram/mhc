use maho_ext_mcp::expose::schema_compat::*;
use serde_json::json;
#[test]
fn supported_schema_keywords_survive_local_ref_resolution() {
    let input = json!({"$schema":"ignored","additionalProperties":false,"type":"object","$defs":{"value":{"type":"string","minLength":1}},"properties":{"x":{"$ref":"#/$defs/value"}}});
    let result = convert_json_schema_to_type_box(&input);
    assert!(result.warnings.is_empty());
    assert_eq!(result.schema["properties"]["x"],json!({"type":"string","minLength":1}));
    assert!(result.schema.get("$schema").is_none());
}
#[test]
fn null_type_is_removed_recursively() {
    let result = convert_json_schema_to_type_box(&json!({"type":null,"properties":{"x":{"type":null},"y":{"type":"null"}}}));
    assert_eq!(result.schema,json!({"properties":{"x":{},"y":{"type":"null"}}}));
}
#[test]
fn unresolvable_ref_uses_permissive_schema() {
    let result = convert_json_schema_to_type_box(&json!({"properties":{"x":{"$ref":"#/$defs/missing"}}}));
    assert_eq!(result.schema,json!({"type":"object","properties":{}})); assert_eq!(result.warnings.len(),1);
}
#[test]
fn output_retry_strips_only_top_level_keys() {
    let result = prepare_output_schema_retry(&json!({"$schema":"ignored","additionalProperties":false,"properties":{"x":{"additionalProperties":false}}}));
    assert_eq!(result.warnings.len(),2); assert_eq!(result.schema["properties"]["x"]["additionalProperties"],false);
}
#[test]
fn all_content_types_and_structured_content_are_preserved() {
    let input = json!({"content":[{"type":"text","text":"hello"},{"type":"image","data":"aW1n","mimeType":"image/png"},{"type":"audio","data":"YXVkaW8=","mimeType":"audio/wav"},{"type":"resource","resource":{"uri":"file:///tmp/a"}},{"type":"resource_link","uri":"https://example.test","name":"Example"}],"structuredContent":{"answer":42}});
    let result = map_mcp_tool_result(&input);
    assert_eq!(result["content"].as_array().unwrap().len(),6);
    for i in 0..5 { assert_eq!(result["content"][i],input["content"][i]); }
    assert_eq!(result["content"][5]["text"],"{\"answer\":42}");
}
#[test]
fn empty_and_error_results_remain_visible() {
    assert_eq!(map_mcp_tool_result(&json!({"content":[]}))["content"][0]["text"],"(empty result)");
    assert_eq!(map_mcp_tool_result(&json!({"isError":true,"content":[{"type":"text","text":"bad"}]}))["error"]["message"],"bad");
}
#[test]
fn naming_sanitizes_caps_and_disambiguates() {
    let entries = [("server/name","first tool"),("very-long-server-name-with-many-segments-and-symbols","very-long-tool-name-with-many-segments-and-symbols"),("alpha-beta","same"),("alpha_beta","same")].map(|(s,t)| McpToolNameEntry { server_name:s.into(),tool_name:t.into() });
    let names = build_mcp_tool_names(&entries,None);
    assert_eq!(names[0],"mcp_server_name_first_tool");
    assert_eq!(names[1],"mcp_very-long-server-name-with-...with-many-segments-and-symbols");
    assert_eq!(names[1].len(),64); assert_ne!(names[2],names[3]);
}
#[tokio::test]
async fn pagination_stops_on_duplicate_cursor() {
    let result = collect_all_pages(|cursor| async move { Ok::<_, std::convert::Infallible>(McpListPage { items:Some(vec![if cursor.is_none() {1} else {2}]),next_cursor:Some("a".into()),..Default::default() }) }).await.unwrap();
    assert_eq!(result.items,[1,2]); assert_eq!(result.pages,2); assert_eq!(result.warnings.len(),1);
}
#[tokio::test]
async fn pagination_stops_at_thousand_pages() {
    let result = collect_all_pages(|cursor| async move { let n = cursor.unwrap_or("0".into()).parse::<usize>().unwrap(); Ok::<_, std::convert::Infallible>(McpListPage { items:Some(vec![n]),next_cursor:Some((n+1).to_string()),..Default::default() }) }).await.unwrap();
    assert_eq!(result.items.len(),1000); assert_eq!(result.warnings.len(),1);
}
