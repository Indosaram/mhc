//! Port of senpi `auth/oauth-fetch.ts`: drop optional OAuth response fields that servers send
//! as `null` or `""` before the body is parsed, so `expires_in: null` never becomes an already
//! expired token and a `null` refresh token is not rejected.
use serde_json::Value;

const OPTIONAL_OAUTH_FIELDS: [&str; 9] = [
    "expires_in",
    "refresh_token",
    "scope",
    "id_token",
    "client_secret",
    "client_secret_expires_at",
    "client_id_issued_at",
    "registration_access_token",
    "registration_client_uri",
];

/// `withoutAbsentOptionalFields`: returns `None` when nothing changed (upstream returns the
/// original body object).
pub fn without_absent_optional_fields(body: &Value) -> Option<Value> {
    let object = body.as_object()?;
    let mut changed = false;
    let mut next = serde_json::Map::new();
    for (key, value) in object {
        if OPTIONAL_OAUTH_FIELDS.contains(&key.as_str())
            && (value.is_null() || value.as_str() == Some(""))
        {
            changed = true;
            continue;
        }
        next.insert(key.clone(), value.clone());
    }
    changed.then_some(Value::Object(next))
}

/// `oauthFetch` body transform: a non-JSON content type or an unparseable body passes through
/// byte-for-byte; a JSON body is re-serialized only when a field was dropped.
pub fn oauth_fetch_body(content_type: Option<&str>, text: String) -> String {
    if !content_type.unwrap_or("").contains("application/json") {
        return text;
    }
    let Ok(body) = serde_json::from_str::<Value>(&text) else {
        return text;
    };
    match without_absent_optional_fields(&body) {
        Some(normalized) => normalized.to_string(),
        None => text,
    }
}

/// `oauthFetch(base)` over reqwest: reads the response and returns its status, headers and
/// normalized body text.
pub async fn oauth_fetch(
    request: reqwest::RequestBuilder,
) -> Result<(reqwest::StatusCode, reqwest::header::HeaderMap, String), reqwest::Error> {
    let response = request.send().await?;
    let status = response.status();
    let headers = response.headers().clone();
    let content_type = headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let text = response.text().await?;
    Ok((
        status,
        headers,
        oauth_fetch_body(content_type.as_deref(), text),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_null_and_empty_optional_fields_only() {
        let text = r#"{"access_token":"a","expires_in":null,"refresh_token":"","scope":"x","token_type":""}"#;
        let out: Value = serde_json::from_str(&oauth_fetch_body(
            Some("application/json; charset=utf-8"),
            text.into(),
        ))
        .expect("json");
        assert_eq!(
            out,
            serde_json::json!({"access_token":"a","scope":"x","token_type":""})
        );
    }

    #[test]
    fn passes_through_unchanged_bodies() {
        let text = r#"{"access_token":"a", "expires_in":3600}"#.to_owned();
        assert_eq!(
            oauth_fetch_body(Some("application/json"), text.clone()),
            text
        );
        assert_eq!(
            oauth_fetch_body(Some("text/plain"), "{\"expires_in\":null}".into()),
            "{\"expires_in\":null}"
        );
        assert_eq!(
            oauth_fetch_body(Some("application/json"), "not json".into()),
            "not json"
        );
        assert_eq!(oauth_fetch_body(None, "[]".into()), "[]");
    }
}
