use maho_server::app_server::user_input_types::*;
use serde_json::json;
#[test]
fn user_input_draft_separates_known_option_labels_from_multiline_other_text() {
    let request = json!({"questions":[{"id":"q","options":[{"label":"known"}]}]});
    let result = read_user_input_result(&json!({"answers":{"q":{"answers":["known","one","two"]},"unknown":{"answers":["ignored"]}},"comment":"note"})).unwrap();
    assert_eq!(to_draft(&request, &result), json!({"answers":{"q":{"selected":["known"],"text":"one\ntwo"}},"comment":"note"}));
    for invalid in [json!(null),json!({"comment":null}),json!({"cancelled":1}),json!({"answers":[]}),json!({"answers":{"q":{"answers":[1]}}})] { assert!(read_user_input_result(&invalid).is_err()); }
}
