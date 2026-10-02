use maho_cli::utils::clipboard_image::*;
#[test]
fn mime_selection_prefers_supported_formats_and_preserves_parameters() {
    let types = ["image/gif", "image/png; charset=binary", "image/jpeg"].map(str::to_owned);
    assert_eq!(select_preferred_image_mime_type(&types).as_deref(), Some("image/png; charset=binary"));
    assert_eq!(extension_for_image_mime_type(" IMAGE/JPEG; charset=binary"), Some("jpg"));
    assert_eq!(extension_for_image_mime_type("text/plain"), None);
}
#[tokio::test]
async fn termux_returns_without_reading_desktop_clipboard() {
    let env = std::collections::BTreeMap::from([("TERMUX_VERSION".to_owned(), "1".to_owned())]);
    assert!(read_clipboard_image(&env, "linux").await.is_none());
}
