use maho_cli::{experimental::radius_auth::*, cli::experimental::command_options::AuthInput};
#[tokio::test]
async fn explicit_tokens_trim_and_files_resolve_from_cwd() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("token"), "  test-value\n").unwrap();
    assert_eq!(explicit_token(Some(&AuthInput::File("token".to_owned())), directory.path().to_str().unwrap(), None).await.unwrap().as_deref(), Some("test-value"));
    assert!(explicit_token(Some(&AuthInput::Token(" ".to_owned())), directory.path().to_str().unwrap(), None).await.is_err());
}
#[tokio::test]
async fn cancellation_is_checked_before_reading() {
    let controller = maho_ai::utils::abort::AbortController::new(); controller.abort(None);
    assert!(explicit_token(None, "/", Some(&controller.signal())).await.is_err());
}
