use maho_cli::{cli::experimental::command_options::AuthInput, experimental::radius_auth::*};
#[tokio::test]
async fn explicit_tokens_trim_and_files_resolve_from_cwd() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("token"), "  test-value\n").unwrap();
    assert_eq!(
        explicit_token(
            Some(&AuthInput::File("token".to_owned())),
            directory.path().to_str().unwrap(),
            None
        )
        .await
        .unwrap()
        .as_deref(),
        Some("test-value")
    );
    assert!(
        explicit_token(
            Some(&AuthInput::Token(" ".to_owned())),
            directory.path().to_str().unwrap(),
            None
        )
        .await
        .is_err()
    );
}
#[tokio::test]
async fn cancellation_is_checked_before_reading() {
    let controller = maho_ai::utils::abort::AbortController::new();
    controller.abort(None);
    assert!(
        explicit_token(None, "/", Some(&controller.signal()))
            .await
            .is_err()
    );
}
fn stored(
    token: Option<&'static str>,
    seen: std::sync::Arc<std::sync::Mutex<Vec<Option<u64>>>>,
) -> StoredRadiusAuth {
    std::sync::Arc::new(move |overrides: maho_ai::models::AuthResolutionOverrides| {
        seen.lock().unwrap().push(overrides.min_oauth_validity_ms);
        Box::pin(std::future::ready(Ok(token.map(|token| {
            maho_ai::models::AuthResolution {
                ambient: false,
                auth: maho_ai::models::ProviderAuthResult {
                    api_key: Some(token.to_owned()),
                    ..Default::default()
                },
                env: None,
            }
        }))))
    })
}
#[tokio::test]
async fn resolver_prefers_the_explicit_token_and_normalizes_the_gateway() {
    let seen = std::sync::Arc::default();
    let resolver = RadiusRelayAuthResolver::with_parts(
        Some(AuthInput::Token(" secret ".to_owned())),
        "radius.example.org/",
        "/",
        false,
        stored(Some("stored"), std::sync::Arc::clone(&seen)),
    );
    let auth = resolver.resolve(true, None).await.unwrap().unwrap();
    assert_eq!(auth.token, "secret");
    assert_eq!(auth.gateway, resolver.gateway());
    assert_eq!(
        resolver.gateway(),
        maho_ai::providers::radius_config::normalize_radius_gateway_url("radius.example.org/")
    );
    assert!(
        seen.lock().unwrap().is_empty(),
        "an explicit token never reads stored credentials"
    );
}
#[tokio::test]
async fn resolver_falls_back_to_the_stored_credential_with_a_five_minute_validity_floor() {
    let seen = std::sync::Arc::default();
    let resolver = RadiusRelayAuthResolver::with_parts(
        None,
        "https://radius.example.org",
        "/",
        false,
        stored(Some("stored"), std::sync::Arc::clone(&seen)),
    );
    assert_eq!(
        resolver.resolve(true, None).await.unwrap().unwrap().token,
        "stored"
    );
    assert_eq!(*seen.lock().unwrap(), [Some(RADIUS_MIN_OAUTH_VALIDITY_MS)]);
}
#[tokio::test]
async fn resolver_without_credentials_fails_only_when_required() {
    let resolver = RadiusRelayAuthResolver::with_parts(
        None,
        "https://radius.example.org",
        "/",
        false,
        stored(None, std::sync::Arc::default()),
    );
    assert_eq!(resolver.resolve(false, None).await.unwrap(), None);
    assert!(
        resolver
            .resolve(true, None)
            .await
            .unwrap_err()
            .contains("/login radius")
    );
}
#[tokio::test]
async fn resolver_is_unavailable_offline() {
    let resolver = RadiusRelayAuthResolver::with_parts(
        Some(AuthInput::Token("secret".to_owned())),
        "https://radius.example.org",
        "/",
        true,
        stored(Some("stored"), std::sync::Arc::default()),
    );
    assert_eq!(resolver.resolve(false, None).await.unwrap(), None);
    assert_eq!(
        resolver.resolve(true, None).await.unwrap_err(),
        "Radius relay connections are unavailable in offline mode"
    );
}
