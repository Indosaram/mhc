use serde_json::{Value, json};

pub fn web_search_item(id: &str, content: &Value) -> Value {
    let raw = &content["raw"];
    let raw_action = &raw["action"];
    let nullable = |key: &str| raw_action.get(key).filter(|value| value.is_string()).cloned().unwrap_or(Value::Null);
    let (action, detail) = match raw_action["type"].as_str() {
        Some("search") => {
            let query = nullable("query");
            let queries = raw_action.get("queries").and_then(Value::as_array).filter(|queries| queries.iter().all(Value::is_string));
            let first = queries.and_then(|queries| queries.first()).and_then(Value::as_str).unwrap_or_default();
            let detail = match query.as_str().filter(|query| !query.is_empty()) {
                Some(query) => query.to_owned(),
                None if !first.is_empty() && queries.is_some_and(|queries| queries.len() > 1) => format!("{first} ..."),
                None => first.to_owned(),
            };
            (json!({"type":"search","query":query,"queries":queries}), detail)
        },
        Some("open_page" | "openPage") => {
            let url = nullable("url");
            let detail = url.as_str().unwrap_or_default().to_owned();
            (json!({"type":"openPage","url":url}), detail)
        },
        Some("find_in_page" | "findInPage") => {
            let url = nullable("url"); let pattern = nullable("pattern");
            let detail = match (pattern.as_str().filter(|value| !value.is_empty()), url.as_str().filter(|value| !value.is_empty())) {
                (Some(pattern), Some(url)) => format!("'{pattern}' in {url}"),
                (Some(pattern), None) => format!("'{pattern}'"),
                (None, url) => url.unwrap_or_default().to_owned(),
            };
            (json!({"type":"findInPage","url":url,"pattern":pattern}), detail)
        },
        _ => (json!({"type":"other"}), String::new()),
    };
    json!({"type":"webSearch","id":id,"query":detail,"action":action,"results":raw.get("results").filter(|value| value.is_array())})
}
