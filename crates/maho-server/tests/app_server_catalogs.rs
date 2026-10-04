use maho_server::app_server::catalogs::paginate_catalog;
use serde_json::json;

#[test]
fn catalogs_validate_scoped_cursor_and_safe_integer_limits() {
    let items = vec![json!({"id":"first"}),json!({"id":"second"})];
    let page = paginate_catalog(&items,&json!({"limit":0}),"catalog/list").unwrap();
    assert_eq!(page["data"],json!([{"id":"first"}]));
    assert_eq!(page["nextCursor"],"1");
    assert_eq!(paginate_catalog(&items,&json!({"cursor":"1"}),"catalog/list").unwrap()["data"],json!([{"id":"second"}]));
    for params in [json!({"cursor":"3"}),json!({"cursor":"-1"}),json!({"cursor":false}),json!({"limit":1.5}),json!({"limit":9007199254740992u64})] {assert_eq!(paginate_catalog(&items,&params,"catalog/list").unwrap_err().code,-32600);}
}
