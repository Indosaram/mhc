use maho_ext_pi_webfetch::webfetch::errors::WebfetchError;
#[test]
fn source_error_class_names_are_preserved() {
    for (error, name) in [
        (
            WebfetchError::InvalidUrl("fixture".into()),
            "InvalidWebfetchUrlError",
        ),
        (WebfetchError::Aborted, "WebfetchAbortError"),
        (WebfetchError::Timeout(3), "WebfetchTimeoutError"),
        (
            WebfetchError::ResponseTooLarge,
            "WebfetchResponseTooLargeError",
        ),
    ] {
        assert_eq!(error.name(), name);
    }
}
#[test]
fn source_error_messages_are_preserved() {
    assert_eq!(WebfetchError::Aborted.to_string(), "Request aborted");
    assert_eq!(
        WebfetchError::Timeout(30).to_string(),
        "Request timed out after 30s"
    );
    assert_eq!(
        WebfetchError::ResponseTooLarge.to_string(),
        "Response too large (exceeds 5MB limit)"
    );
    assert_eq!(
        WebfetchError::InvalidUrl("Invalid URL: http://".into()).to_string(),
        "Invalid URL: http://"
    );
    assert_eq!(
        WebfetchError::AbortReason("custom".into()).name(),
        "AbortError"
    );
}
