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

// senpi image-resize-core.ts `resizeImageInProcess` strategy: within limits passes through; an
// oversized image keeps its aspect ratio at the first fitting size; undecodable bytes give null.
#[test]
fn resize_core_passes_through_scales_proportionally_and_rejects_garbage() {
    use maho_cli::utils::image_resize_core::{ImageResizeOptions, resize_image_in_process};
    let png = photon_rs::PhotonImage::new(vec![200; 40 * 20 * 4], 40, 20).get_bytes();
    let fitting = resize_image_in_process(&png, "", &ImageResizeOptions::default()).expect("decodable");
    assert!(!fitting.was_resized);
    assert_eq!((fitting.mime_type.as_str(), fitting.width, fitting.height), ("image/png", 40, 20));
    let bounded = ImageResizeOptions { max_width: 10, ..Default::default() };
    let scaled = resize_image_in_process(&png, "image/png", &bounded).expect("resized");
    assert!(scaled.was_resized);
    assert_eq!((scaled.original_width, scaled.original_height, scaled.width, scaled.height), (40, 20, 10, 5));
    assert!(resize_image_in_process(b"not an image", "image/png", &ImageResizeOptions::default()).is_none());
}
