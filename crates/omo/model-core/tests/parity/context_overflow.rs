use model_core::is_context_overflow_message;

#[test]
fn recognizes_provider_overflow_exceeds_context_window() {
    assert!(is_context_overflow_message(
        "Your input exceeds the context window of this model"
    ));
}

#[test]
fn recognizes_provider_overflow_prompt_too_long() {
    assert!(is_context_overflow_message(
        "prompt is too long: 213462 tokens > 200000 maximum"
    ));
}

#[test]
fn recognizes_provider_overflow_token_count_exceeds_maximum() {
    assert!(is_context_overflow_message(
        "The input token count (1196265) exceeds the maximum number of tokens allowed (1048575)"
    ));
}

#[test]
fn recognizes_provider_overflow_maximum_prompt_length() {
    assert!(is_context_overflow_message(
        "This model's maximum prompt length is 131072 but the request contains 537812 tokens"
    ));
}

#[test]
fn recognizes_provider_overflow_reduce_length_messages() {
    assert!(is_context_overflow_message(
        "Please reduce the length of the messages or completion"
    ));
}

#[test]
fn recognizes_provider_overflow_413_body_too_large() {
    assert!(is_context_overflow_message("413 Request body too large"));
}

#[test]
fn recognizes_provider_overflow_input_length_exceeds_context() {
    assert!(is_context_overflow_message(
        "Input length (265330) exceeds model's maximum context length (262144)."
    ));
}

#[test]
fn rejects_non_overflow_failure_429() {
    assert!(!is_context_overflow_message("429: too many requests"));
}

#[test]
fn rejects_non_overflow_failure_400() {
    assert!(!is_context_overflow_message("400: invalid request"));
}

#[test]
fn rejects_non_overflow_failure_503() {
    assert!(!is_context_overflow_message(
        "503: All providers are temporarily cooling down"
    ));
}
