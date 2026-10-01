use maho_server::app_server::approval_redaction::*;
use serde_json::json;
#[test]
fn redaction_preserves_public_and_malformed_answers_and_replaces_secret_array_elements() {
    let ids = read_secret_question_ids(&json!({"questions":[{"id":"secret","isSecret":true},{"id":"snake","is_secret":true},{"id":"public","isSecret":"true"},{"id":"","isSecret":true},null]}));
    assert_eq!(ids.len(), 2);
    let response = json!({"extra":true,"answers":{"secret":{"answers":["hidden",3],"extra":1},"snake":{"answers":"unchanged"},"public":{"answers":["visible"]}}});
    assert_eq!(redact_secret_answers(response, &ids), json!({"extra":true,"answers":{"secret":{"answers":["[REDACTED]","[REDACTED]"],"extra":1},"snake":{"answers":"unchanged"},"public":{"answers":["visible"]}}}));
    assert_eq!(redact_secret_answers(json!([1]), &ids), json!([1]));
}
