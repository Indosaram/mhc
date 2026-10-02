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
fn tiny_red_bmp() -> Vec<u8> {
    let mut bytes = vec![0; 58]; bytes[..2].copy_from_slice(b"BM");
    for (offset, value) in [(2, 58u32), (10, 54), (14, 40), (18, 1), (22, 1), (30, 0), (34, 4)] { bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes()); }
    bytes[26..28].copy_from_slice(&1u16.to_le_bytes()); bytes[28..30].copy_from_slice(&24u16.to_le_bytes()); bytes[56] = 255; bytes
}
#[test]
fn upstream_detects_bmp_magic() { assert_eq!(maho_cli::utils::mime::detect_supported_image_mime_type(&tiny_red_bmp()), Some("image/bmp")); }
#[tokio::test]
async fn upstream_converts_bmp_without_resize() {
    use base64::Engine;
    let image = process_image(&tiny_red_bmp(), "image/bmp", ProcessImageOptions { auto_resize_images: Some(false), ..Default::default() }).await.unwrap();
    assert_eq!(image.mime_type, "image/png");
    assert_eq!(image.hints.len(), 1);
    assert!(base64::engine::general_purpose::STANDARD.decode(&image.data).unwrap().starts_with(b"\x89PNG"));
}
#[tokio::test]
async fn upstream_converts_bmp_before_resize() {
    use base64::Engine;
    let image = process_image(&tiny_red_bmp(), "image/bmp", Default::default()).await.unwrap();
    assert_eq!(image.mime_type, "image/png"); assert_eq!(image.hints.len(), 1);
    assert!(base64::engine::general_purpose::STANDARD.decode(&image.data).unwrap().starts_with(b"\x89PNG"));
}
