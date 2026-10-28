use serde_json::Value;
pub fn answered_ui_request_id(frame: &Value) -> Option<&str> {
    frame["uiRequestId"]
        .as_str()
        .or_else(|| frame["id"].as_str())
}
pub fn settled_question_status(answers: &Value, comment: Option<&str>) -> Option<&'static str> {
    if comment.is_some_and(|comment| !comment.trim().is_empty()) {
        Some("comment-submitted")
    } else if answers
        .as_object()
        .is_some_and(|answers| !answers.is_empty())
    {
        Some("answered")
    } else {
        None
    }
}
pub fn unanswered_question_ids(questions: &[Value], answers: &Value) -> Vec<String> {
    questions
        .iter()
        .filter_map(|question| {
            let id = question["id"].as_str()?;
            let answer = &answers[id];
            (answer["selected"].as_array().is_none_or(Vec::is_empty)
                && answer["text"]
                    .as_str()
                    .is_none_or(|text| text.trim().is_empty()))
            .then(|| id.to_owned())
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn correlation_is_not_the_answered_request_id() {
        assert_eq!(
            answered_ui_request_id(&json!({"id":"reply","uiRequestId":"request"})),
            Some("request")
        );
        assert_eq!(
            answered_ui_request_id(&json!({"id":"legacy"})),
            Some("legacy")
        );
    }
    #[test]
    fn comment_settles_but_blank_input_does_not() {
        assert_eq!(
            settled_question_status(&json!({}), Some(" guidance ")),
            Some("comment-submitted")
        );
        assert_eq!(settled_question_status(&json!({}), Some(" \t")), None);
        assert_eq!(
            unanswered_question_ids(
                &[json!({"id":"a"}), json!({"id":"b"})],
                &json!({"a":{"selected":["one"]},"b":{"text":" "}})
            ),
            vec!["b"]
        );
    }
}
