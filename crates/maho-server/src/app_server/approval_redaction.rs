use serde_json::Value;
use std::collections::BTreeSet;

pub fn read_secret_question_ids(params: &Value) -> BTreeSet<String> {
    params.get("questions").and_then(Value::as_array).into_iter().flatten().filter_map(|question| {
        let question = question.as_object()?;
        if question.get("isSecret") != Some(&Value::Bool(true)) && question.get("is_secret") != Some(&Value::Bool(true)) { return None; }
        question.get("id").and_then(Value::as_str).filter(|id| !id.is_empty()).map(str::to_owned)
    }).collect()
}
pub fn redact_secret_answers(mut response: Value, secret_question_ids: &BTreeSet<String>) -> Value {
    if secret_question_ids.is_empty() { return response; }
    if let Some(answers) = response.get_mut("answers").and_then(Value::as_object_mut) {
        for (id, answer) in answers {
            if secret_question_ids.contains(id) && let Some(values) = answer.get_mut("answers").and_then(Value::as_array_mut) {
                for value in values { *value = Value::String("[REDACTED]".into()); }
            }
        }
    }
    response
}
