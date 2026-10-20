//! Port of senpi `core/model-discovery-url.ts`.
pub fn listing_url(base: &str) -> Result<(url::Url, String), String> {
    let mut request = url::Url::parse(base.trim()).map_err(|_| "Invalid provider baseUrl")?;
    request.set_path(&format!("{}/models", request.path().trim_end_matches('/')));
    let mut display = request.clone();
    display
        .set_username("")
        .map_err(|_| "Invalid provider baseUrl")?;
    display
        .set_password(None)
        .map_err(|_| "Invalid provider baseUrl")?;
    display.set_fragment(None);
    let keys: Vec<_> = request
        .query_pairs()
        .map(|(key, _)| key.into_owned())
        .collect();
    display.set_query(None);
    let display = if keys.is_empty() {
        display.to_string()
    } else {
        let query = keys
            .iter()
            .map(|key| {
                format!(
                    "{}=<redacted>",
                    percent_encoding::utf8_percent_encode(key, percent_encoding::NON_ALPHANUMERIC)
                )
            })
            .collect::<Vec<_>>()
            .join("&");
        format!("{display}?{query}")
    };
    Ok((request, display))
}

#[cfg(test)]
mod tests {
    #[test]
    fn listing_preserves_request_secrets_but_redacts_display() {
        // Given a configured endpoint carrying credentials and query routing.
        let base = "https://user:secret@example.com/v1/?key=token#fragment";
        // When deriving its model listing URL.
        let (request, display) = super::listing_url(base).expect("listing URL");
        // Then the request retains configuration while diagnostics contain no secret.
        assert_eq!(
            request.as_str(),
            "https://user:secret@example.com/v1/models?key=token#fragment"
        );
        assert_eq!(display, "https://example.com/v1/models?key=<redacted>");
    }
}
