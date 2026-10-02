use maho_ai::types::{ContentBlock, ImageContent};
use maho_cli::utils::tool_result_images::*;
use base64::Engine;
#[test]
fn buffer_base64_accepts_url_alphabet_noise_and_partial_groups() {
    use maho_cli::utils::image_convert::decode_base64;
    assert_eq!(decode_base64("Y! W\nJjZA"), b"abcd");
    assert_eq!(decode_base64("YQb"), [97, 6]);
    assert_eq!(decode_base64("YQbAA"), [97, 6, 192]);
    assert_eq!(decode_base64("-_8"), [251, 255]);
    assert_eq!(decode_base64("YQ==ignored"), b"a");
}
#[tokio::test]
async fn unchanged_text_and_failed_images_borrow_original_content() {
    let content = vec![ContentBlock::text("text")];
    assert!(matches!(normalize_tool_result_images(&content, None).await, std::borrow::Cow::Borrowed(_)));
    let failed = vec![ContentBlock::Image(ImageContent { data: "aW52YWxpZA==".to_owned(), mime_type: "image/tiff".to_owned() })];
    assert!(matches!(normalize_tool_result_images(&failed, None).await, std::borrow::Cow::Borrowed(_)));
}
#[tokio::test]
async fn normalization_rewrites_image_and_appends_conversion_hint() {
    let bytes = photon_rs::PhotonImage::new(vec![255; 4], 1, 1).get_bytes();
    let content = vec![ContentBlock::Image(ImageContent { data: base64::engine::general_purpose::STANDARD.encode(bytes), mime_type: "image/tiff".to_owned() })];
    let result = normalize_tool_result_images(&content, Some(false)).await;
    assert!(matches!(result, std::borrow::Cow::Owned(_)));
    assert_eq!(result.len(), 2);
    assert!(matches!(&result[0], ContentBlock::Image(image) if image.mime_type == "image/png"));
    assert!(matches!(&result[1], ContentBlock::Text(_)));
}
