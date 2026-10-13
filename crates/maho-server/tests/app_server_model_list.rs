use maho_server::app_server::model_list::*;
use serde_json::json;
#[test]
fn model_pagination_filters_hidden_before_cursor_and_rejects_invalid_scope() {
    let models = vec![json!({"id":"one","hidden":false}),json!({"id":"hidden","hidden":true}),json!({"id":"two","hidden":false})];
    assert_eq!(build_model_list_response(&models, &json!({"limit":1})).unwrap(), json!({"data":[models[0]],"nextCursor":"1"}));
    assert_eq!(build_model_list_response(&models, &json!({"cursor":"1"})).unwrap()["data"], json!([models[2]]));
    assert!(build_model_list_response(&models, &json!({"cursor":"3"})).is_err());
    assert!(build_model_list_response(&models, &json!({"cursor":" 1"})).is_err());
    let wire = build_wire_model(&json!({"provider":"p","id":"m","reasoning":true}), &["off".into(),"medium".into()], Some("m"));
    assert_eq!(wire["id"], "p/m");
    assert_eq!(wire["supportedReasoningEfforts"].as_array().unwrap().len(), 1);
    assert_eq!(wire["isDefault"], true);
    assert_eq!(build_model_list_response(&models, &json!({"limit":1.5})).unwrap()["data"], json!([models[0]]));
}

#[test]
fn empty_catalog_limit_is_zero_and_fractional_limits_keep_a_js_number_cursor() {
    // Pinned model-list.ts: normalizeModelListLimit returns 0 for an empty list, and
    // nextCursor is String(end) where end = Math.min(start + limit, total) may be fractional.
    assert_eq!(build_model_list_response(&[], &json!({"limit":5})).unwrap(), json!({"data":[],"nextCursor":null}));
    let models = vec![json!({"id":"a"}),json!({"id":"b"}),json!({"id":"c"}),json!({"id":"d"})];
    let page = build_model_list_response(&models, &json!({"limit":2.5})).unwrap();
    assert_eq!(page["data"], json!([models[0], models[1]]));
    assert_eq!(page["nextCursor"], "2.5");
    // RegExp.test coerces a numeric cursor to its digit string, so 1 is accepted like "1".
    assert_eq!(build_model_list_response(&models, &json!({"cursor":1})).unwrap()["data"][0], models[1]);
}
