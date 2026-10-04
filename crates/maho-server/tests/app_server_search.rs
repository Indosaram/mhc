use maho_server::app_server::{search::{literal_snippet,search_window},search_cache::SearchSessionRecord,search_params::parse_search_params};
use serde_json::json;
use std::collections::BTreeSet;

#[test]
fn search_cursor_scopes_query_and_reverses_from_inclusive_anchor() {
    let records = (0..3).map(|id|SearchSessionRecord {thread:json!({"id":id.to_string(),"createdAt":"2020-01-01T00:00:00.000Z"}),recency_at:"2020-01-01T00:00:00.000Z".into(),searchable_text:"Hello".into()}).collect::<Vec<_>>();
    let mut params = parse_search_params(&json!({"searchTerm":"hello","sourceKinds":["appServer"],"sortDirection":"asc","limit":1})).unwrap();
    let page = search_window(&mut records.clone(),&params,&BTreeSet::new()).unwrap();
    params.cursor = page["nextCursor"].as_str().map(str::to_owned);
    assert_eq!(search_window(&mut records.clone(),&params,&BTreeSet::new()).unwrap()["start"],1);
    params.search_term = "other".into();
    assert_eq!(search_window(&mut records.clone(),&params,&BTreeSet::new()).unwrap_err().code,-32600);
    params.search_term = "hello".into();params.cursor = page["backwardsCursor"].as_str().map(str::to_owned);params.sort_direction = "desc".into();
    assert_eq!(search_window(&mut records.clone(),&params,&BTreeSet::new()).unwrap()["start"],2);
    assert_eq!(literal_snippet("Hello world","hello"),"Hello world");
}
