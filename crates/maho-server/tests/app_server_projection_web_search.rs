use maho_server::app_server::projection_web_search::web_search_item;
use serde_json::json;
#[test]
fn web_search_projection_normalizes_action_aliases_nullable_fields_and_detail() {
    assert_eq!(web_search_item("id", &json!({"raw":{"action":{"type":"search","query":"","queries":["one","two"]},"results":[{"title":"result"}]}})), json!({"type":"webSearch","id":"id","query":"one ...","action":{"type":"search","query":"","queries":["one","two"]},"results":[{"title":"result"}]}));
    assert_eq!(web_search_item("id", &json!({"raw":{"action":{"type":"find_in_page","url":"page","pattern":"needle"}}}))["query"], "'needle' in page");
    assert_eq!(web_search_item("id", &json!({"raw":{"action":{"type":"open_page","url":"page"}}}))["action"]["type"], "openPage");
    assert_eq!(web_search_item("id", &json!({"raw":{"action":{"type":"search","queries":[1]}}}))["action"]["queries"], json!(null));
    assert_eq!(web_search_item("id", &json!({"raw":null}))["action"], json!({"type":"other"}));
}
