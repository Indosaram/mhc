use maho_cli::utils::image_process::*;
#[tokio::test]
async fn supported_mime_alias_and_disabled_resize_preserve_input() {
    let result = process_image(b"bytes", " IMAGE/JPG; parameter=1", ProcessImageOptions { auto_resize_images: Some(false), ..Default::default() }).await.unwrap();
    assert_eq!(result.mime_type, "image/jpeg");
    assert_eq!(result.data, "Ynl0ZXM=");
    assert!(result.hints.is_empty());
}
#[tokio::test]
async fn image_conversion_and_resize_produce_coordinate_hint() {
    let bytes = photon_rs::PhotonImage::new(vec![255; 16 * 8 * 4], 16, 8).get_bytes();
    let result = process_image(&bytes, "image/tiff", ProcessImageOptions { resize_options: Some(maho_cli::utils::image_resize::ImageResizeOptions { max_width: 8, ..Default::default() }), ..Default::default() }).await.unwrap();
    assert_eq!(result.mime_type, "image/png");
    assert_eq!(result.hints.len(), 2);
    assert!(result.hints[0].contains("image/tiff"));
    assert!(result.hints[1].contains("2.00"));
}
#[tokio::test]
async fn unsupported_invalid_bytes_are_reported_as_omitted() {
    assert!(process_image(b"invalid", "image/tiff", Default::default()).await.is_err());
}
