//! Port of senpi packages/ai/src/utils/tool-choice-fallback.ts.

use crate::types::Model;
use futures::{Stream, StreamExt};
use regex::Regex;
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::future::Future;
use std::sync::{LazyLock, Mutex};

static PATTERNS: LazyLock<[Regex; 6]> = LazyLock::new(|| {
    let compile = |p: &str| Regex::new(p).unwrap_or_else(|e| panic!("{e}"));
    [
        compile(
            r"(?is)tool[_\s-]?choices?\b.*?(not\s+(?:currently\s+)?compatible|incompatible|not\s+(?:currently\s+)?supported|unsupported)",
        ),
        compile(
            r"(?is)forces?\s+tool\s+use.*?(not\s+(?:currently\s+)?compatible|incompatible|not\s+(?:currently\s+)?supported|unsupported)",
        ),
        compile(r"(?is)does\s+not\s+support\s+(?:forced|this|the)\s+tool[_\s-]?choices?"),
        compile(r#"(?is)\bonly\s+[`"]*auto(?:matic)?[`"]*\s.{0,40}?tool[_\s-]?choice"#),
        compile(
            r"(?is)thinking\s+may\s+not\s+be\s+enabled\s+when\s+tool[_\s-]?choice\s+forces\s+tool\s+use",
        ),
        compile(r"(?is)tool[_\s-]?choice\s+cannot\s+force\s+tool\s+use"),
    ]
});

/// A provider HTTP failure: status (or `response.status`) plus message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpFailure {
    pub status: Option<u16>,
    pub message: String,
}

impl HttpFailure {
    pub fn from_value(error: &Value) -> Self {
        let status = error
            .get("status")
            .or_else(|| error.pointer("/response/status"))
            .and_then(Value::as_u64)
            .and_then(|status| u16::try_from(status).ok());
        let message = error
            .as_str()
            .or_else(|| error.get("message").and_then(Value::as_str))
            .map(str::to_owned)
            .unwrap_or_else(|| error.to_string());
        Self { status, message }
    }
}

pub fn is_forced_tool_choice_unsupported_error(
    error: &HttpFailure,
    sent_forced_tool_choice: bool,
) -> bool {
    if !sent_forced_tool_choice || error.status != Some(400) {
        return false;
    }
    PATTERNS
        .iter()
        .any(|pattern| pattern.is_match(&error.message))
}

pub fn omit_tool_choice_param(params: &Map<String, Value>) -> Map<String, Value> {
    let mut next = params.clone();
    next.remove("tool_choice");
    next
}

pub fn is_request_refusal(error: &HttpFailure, in_band_before_content: bool) -> bool {
    error.status == Some(400) || (error.status.is_none() && in_band_before_content)
}

pub fn refusal_names_forced_tool(error: &HttpFailure, params: &Map<String, Value>) -> bool {
    if !is_request_refusal(error, false) {
        return false;
    }
    let Some(forced) = params
        .get("tool_choice")
        .and_then(tool_name)
        .filter(|name| !name.is_empty())
    else {
        return false;
    };
    if let Some(index) = params
        .get("tools")
        .and_then(Value::as_array)
        .and_then(|tools| {
            tools
                .iter()
                .position(|tool| tool_name(tool) == Some(forced))
        })
    {
        let pattern = format!(r"\btools(?:\.{index}|\[{index}\])");
        if let Ok(pattern) = Regex::new(&pattern) {
            for matched in pattern.find_iter(&error.message) {
                if !error.message[matched.end()..].starts_with(|c: char| c.is_ascii_digit()) {
                    return true;
                }
            }
        }
    }
    ['\'', '"', '`']
        .iter()
        .any(|quote| error.message.contains(&format!("{quote}{forced}{quote}")))
}

fn tool_name(value: &Value) -> Option<&str> {
    value
        .get("function")
        .unwrap_or(value)
        .get("name")
        .and_then(Value::as_str)
}

#[derive(Debug)]
pub struct ForcedToolChoiceFailure {
    pub failure: HttpFailure,
    pub in_band_before_content: bool,
}

impl From<HttpFailure> for ForcedToolChoiceFailure {
    fn from(failure: HttpFailure) -> Self {
        Self {
            failure,
            in_band_before_content: false,
        }
    }
}

static REFUSALS: LazyLock<Mutex<HashSet<[String; 4]>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

fn refusal_key(model: &Model) -> [String; 4] {
    [
        model.api.clone(),
        model.provider.clone(),
        model.base_url.clone(),
        model.id.clone(),
    ]
}

pub fn has_refused_forced_tool_choice(model: &Model) -> bool {
    REFUSALS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .contains(&refusal_key(model))
}

pub fn clear_forced_tool_choice_refusals() {
    REFUSALS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clear();
}

pub async fn send_with_forced_tool_choice_fallback<T, F, Fut>(
    target: &Model,
    params: Map<String, Value>,
    accepts_forced_tool_choice: bool,
    is_forced: impl Fn(Option<&Value>) -> bool,
    mut send: F,
) -> Result<(Map<String, Value>, T), ForcedToolChoiceFailure>
where
    F: FnMut(Map<String, Value>) -> Fut,
    Fut: Future<Output = Result<T, ForcedToolChoiceFailure>>,
{
    send_with_forced_tool_choice_fallback_classified(
        target,
        params,
        accepts_forced_tool_choice,
        is_forced,
        &mut send,
        |error: &ForcedToolChoiceFailure| ForcedToolChoiceFailure {
            failure: error.failure.clone(),
            in_band_before_content: error.in_band_before_content,
        },
    )
    .await
}

/// Runs the shared fallback policy without erasing the adapter's typed failure metadata.
pub async fn send_with_forced_tool_choice_fallback_classified<T, E, F, Fut>(
    target: &Model,
    params: Map<String, Value>,
    accepts_forced_tool_choice: bool,
    is_forced: impl Fn(Option<&Value>) -> bool,
    mut send: F,
    classify: impl Fn(&E) -> ForcedToolChoiceFailure,
) -> Result<(Map<String, Value>, T), E>
where
    F: FnMut(Map<String, Value>) -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    let forced = is_forced(params.get("tool_choice"));
    if forced && (!accepts_forced_tool_choice || has_refused_forced_tool_choice(target)) {
        let params = omit_tool_choice_param(&params);
        let result = send(params.clone()).await?;
        return Ok((params, result));
    }
    match send(params.clone()).await {
        Ok(result) => Ok((params, result)),
        Err(error) => {
            let classified = classify(&error);
            let mut refusal = classified.failure;
            if is_request_refusal(&refusal, classified.in_band_before_content) {
                refusal.status = Some(400);
            }
            if !is_forced_tool_choice_unsupported_error(&refusal, forced)
                && !(forced && refusal_names_forced_tool(&refusal, &params))
            {
                return Err(error);
            }
            let params = omit_tool_choice_param(&params);
            let result = send(params.clone()).await?;
            if !refusal.message.to_lowercase().contains("thinking") {
                REFUSALS
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .insert(refusal_key(target));
            }
            Ok((params, result))
        }
    }
}

pub struct PrimedStreamError<E> {
    pub error: E,
    pub in_band_before_content: bool,
}

pub async fn prime_stream_until_content<S, T, E>(
    stream: S,
    has_content: impl Fn(&T) -> bool,
    max_buffered: usize,
) -> Result<impl Stream<Item = Result<T, E>>, PrimedStreamError<E>>
where
    S: Stream<Item = Result<T, E>> + Unpin,
{
    let mut stream = stream.fuse();
    let mut buffered = Vec::new();
    while buffered.len() < max_buffered {
        match stream.next().await {
            None => break,
            Some(Err(error)) => {
                return Err(PrimedStreamError {
                    error,
                    in_band_before_content: true,
                });
            }
            Some(Ok(event)) => {
                let content = has_content(&event);
                buffered.push(Ok(event));
                if content {
                    break;
                }
            }
        }
    }
    Ok(futures::stream::iter(buffered).chain(stream))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_status_and_current_rejections_are_recognized() {
        let refusal = HttpFailure::from_value(
            &serde_json::json!({"response":{"status":400},"message":"only automatic tool choice is supported"}),
        );
        assert!(is_forced_tool_choice_unsupported_error(&refusal, true));
        let unrelated = HttpFailure {
            status: Some(400),
            message: "does not support temperature alongside tool_choice".into(),
        };
        assert!(!is_forced_tool_choice_unsupported_error(&unrelated, true));
    }

    #[test]
    fn forced_tool_indices_do_not_match_other_tools() {
        let params = serde_json::json!({"tool_choice": {"type":"function", "name":"first"}, "tools":[{"name":"first"},{"name":"second"}]}).as_object().expect("params").clone();
        for message in [
            "tools.0 schema invalid",
            "tools[0] schema invalid",
            "'first' schema invalid",
        ] {
            assert!(refusal_names_forced_tool(
                &HttpFailure {
                    status: Some(400),
                    message: message.into()
                },
                &params
            ));
        }
        for message in [
            "tools.01 schema invalid",
            "tools[1] schema invalid",
            "'second' schema invalid",
        ] {
            assert!(!refusal_names_forced_tool(
                &HttpFailure {
                    status: Some(400),
                    message: message.into()
                },
                &params
            ));
        }
    }

    #[tokio::test]
    async fn successful_fallback_is_remembered_for_the_wire_target() {
        let mut model = crate::models_generated::get_builtin_provider_models("openai")
            .expect("models")[0]
            .clone();
        model.base_url = "https://forced-fallback-memory.test".into();
        let params = serde_json::json!({"tool_choice":"required"})
            .as_object()
            .expect("params")
            .clone();
        let mut calls = 0;
        let result = send_with_forced_tool_choice_fallback(
            &model,
            params.clone(),
            true,
            |choice| choice == Some(&Value::from("required")),
            |body| {
                calls += 1;
                std::future::ready(if body.contains_key("tool_choice") {
                    Err(HttpFailure {
                        status: Some(400),
                        message: "tool_choice not currently supported".into(),
                    }
                    .into())
                } else {
                    Ok(())
                })
            },
        )
        .await
        .expect("fallback");
        assert_eq!(calls, 2);
        assert!(!result.0.contains_key("tool_choice"));
        let result = send_with_forced_tool_choice_fallback(
            &model,
            params,
            true,
            |_| true,
            |body| std::future::ready(Ok(body.contains_key("tool_choice"))),
        )
        .await
        .expect("remembered");
        assert!(!result.1);
    }

    #[tokio::test]
    async fn thinking_refusals_are_not_remembered() {
        let mut model = crate::models_generated::get_builtin_provider_models("openai")
            .expect("models")[0]
            .clone();
        model.base_url = "https://forced-fallback-thinking.test".into();
        let params = serde_json::json!({"tool_choice":"required"})
            .as_object()
            .expect("params")
            .clone();
        send_with_forced_tool_choice_fallback(
            &model,
            params,
            true,
            |_| true,
            |body| {
                std::future::ready(if body.contains_key("tool_choice") {
                    Err(HttpFailure {
                        status: Some(400),
                        message: "Thinking mode does not support this tool_choice".into(),
                    }
                    .into())
                } else {
                    Ok(())
                })
            },
        )
        .await
        .expect("fallback");
        assert!(!has_refused_forced_tool_choice(&model));
    }

    #[tokio::test]
    async fn priming_replays_contentless_events_in_order() {
        let input = futures::stream::iter([Ok::<_, ()>(0), Ok(0), Ok(1), Ok(2)]);
        let primed = prime_stream_until_content(input, |event| *event > 0, 64).await;
        let Ok(primed) = primed else {
            panic!("priming failed");
        };
        assert_eq!(
            primed.collect::<Vec<_>>().await,
            vec![Ok(0), Ok(0), Ok(1), Ok(2)]
        );
    }

    #[tokio::test]
    async fn priming_stops_at_the_contentless_event_bound() {
        // Given
        let input = futures::stream::iter([Ok::<_, &str>(0), Ok(0), Err("later refusal")]);

        // When
        let result = prime_stream_until_content(input, |_| false, 2).await;

        // Then
        let Ok(stream) = result else { panic!("events beyond the bound must not be primed"); };
        assert_eq!(stream.collect::<Vec<_>>().await, vec![Ok(0), Ok(0), Err("later refusal")]);
    }

    #[tokio::test]
    async fn explicit_server_failure_is_not_reclassified_as_a_request_refusal() {
        // Given
        let mut model = crate::models_generated::get_builtin_provider_models("openai").expect("models")[0].clone();
        model.id = "forced-choice-in-band-server-status".into();
        let params = serde_json::json!({"tool_choice":"required"}).as_object().expect("params").clone();
        let mut calls = 0;

        // When
        let result = send_with_forced_tool_choice_fallback(&model, params, true, |_| true, |_| {
            calls += 1;
            std::future::ready(Err::<(), _>(ForcedToolChoiceFailure {
                failure: HttpFailure { status: Some(503), message: "tool_choice not supported".into() },
                in_band_before_content: true,
            }))
        }).await;

        // Then
        assert!(matches!(result, Err(error) if error.failure.status == Some(503)));
        assert_eq!(calls, 1);
        assert!(!has_refused_forced_tool_choice(&model));
    }

    #[test]
    fn detects_forced_tool_choice_rejections() {
        let err = |status, message: &str| HttpFailure {
            status,
            message: message.into(),
        };
        assert!(is_forced_tool_choice_unsupported_error(
            &err(Some(400), "tool_choice is not supported"),
            true
        ));
        assert!(is_forced_tool_choice_unsupported_error(
            &err(Some(400), "Thinking forces tool use\nincompatible"),
            true
        ));
        assert!(is_forced_tool_choice_unsupported_error(
            &err(Some(400), "model does not support forced tool choice"),
            true
        ));
        assert!(!is_forced_tool_choice_unsupported_error(
            &err(Some(400), "tool_choice is not supported"),
            false
        ));
        assert!(!is_forced_tool_choice_unsupported_error(
            &err(Some(500), "tool_choice is not supported"),
            true
        ));
        let mut params = Map::new();
        params.insert("tool_choice".into(), Value::from("any"));
        params.insert("model".into(), Value::from("m"));
        assert_eq!(omit_tool_choice_param(&params).len(), 1);
    }
}
