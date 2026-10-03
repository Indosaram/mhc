use maho_cli::utils::image_resize::*;
use base64::Engine;
#[tokio::test]
async fn preserves_small_images_and_resizes_real_png_without_mutating_input() {
    let image = photon_rs::PhotonImage::new(vec![255; 16 * 8 * 4], 16, 8);
    let bytes = image.get_bytes();
    let original = bytes.clone();
    let small = resize_image(&bytes, "image/png", Default::default()).await.unwrap();
    assert!(!small.was_resized);
    assert_eq!(base64::engine::general_purpose::STANDARD.decode(small.data).unwrap(), bytes);
    let resized = resize_image(&bytes, "image/png", ImageResizeOptions { max_width: 8, max_height: 8, ..Default::default() }).await.unwrap();
    assert_eq!((resized.width, resized.height), (8, 4));
    assert!(resized.was_resized);
    let decoded = photon_rs::PhotonImage::new_from_byteslice(base64::engine::general_purpose::STANDARD.decode(resized.data.clone()).unwrap());
    assert_eq!((decoded.get_width(), decoded.get_height()), (8, 4));
    assert_eq!(bytes, original);
    assert!(format_dimension_note(&resized).unwrap().contains("2.00"));
}
#[tokio::test]
async fn returns_none_for_invalid_image_or_impossible_byte_budget() {
    assert!(resize_image(b"invalid", "image/png", Default::default()).await.is_none());
    let bytes = photon_rs::PhotonImage::new(vec![255; 4], 1, 1).get_bytes();
    assert!(resize_image(&bytes, "image/png", ImageResizeOptions { max_bytes: 1.0, ..Default::default() }).await.is_none());
}
