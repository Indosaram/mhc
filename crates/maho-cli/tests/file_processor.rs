use maho_cli::cli::file_processor::*;
#[tokio::test]
async fn reads_text_strips_bom_and_skips_empty_files() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("text"), "\u{feff}hello").unwrap();
    std::fs::write(directory.path().join("empty"), b"").unwrap();
    let result = process_file_arguments(&["text".to_owned(), "empty".to_owned()], directory.path(), None).await.unwrap();
    assert_eq!(result.text, format!("<file name=\"{}\">\nhello\n</file>\n", directory.path().join("text").display()));
    assert!(result.images.is_empty());
    assert!(process_file_arguments(&["missing".to_owned()], directory.path(), None).await.is_err());
}
#[tokio::test]
async fn processes_real_image_as_attachment() {
    let directory = tempfile::tempdir().unwrap();
    let bytes = photon_rs::PhotonImage::new(vec![255; 4], 1, 1).get_bytes();
    std::fs::write(directory.path().join("image.png"), bytes).unwrap();
    let result = process_file_arguments(&["image.png".to_owned()], directory.path(), None).await.unwrap();
    assert_eq!(result.images.len(), 1);
    assert_eq!(result.images[0].mime_type, "image/png");
}
// senpi block-images.test.ts `processFileArguments` and image-resize-callers.test.ts deferral case.
const TINY_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==";
fn tiny_png() -> Vec<u8> { use base64::Engine; base64::engine::general_purpose::STANDARD.decode(TINY_PNG_BASE64).expect("tiny png") }
fn tiny_bmp_1x1_red_24bpp() -> Vec<u8> {
    let mut buffer = vec![0u8; 58];
    buffer[0..2].copy_from_slice(b"BM");
    buffer[2..6].copy_from_slice(&58u32.to_le_bytes());
    buffer[10..14].copy_from_slice(&54u32.to_le_bytes());
    buffer[14..18].copy_from_slice(&40u32.to_le_bytes());
    buffer[18..22].copy_from_slice(&1i32.to_le_bytes());
    buffer[22..26].copy_from_slice(&1i32.to_le_bytes());
    buffer[26..28].copy_from_slice(&1u16.to_le_bytes());
    buffer[28..30].copy_from_slice(&24u16.to_le_bytes());
    buffer[34..38].copy_from_slice(&4u32.to_le_bytes());
    buffer[56] = 0xff;
    buffer
}
#[tokio::test]
async fn upstream_always_processes_images() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("test.png"), tiny_png()).unwrap();
    let result = process_file_arguments(&["test.png".to_owned()], directory.path(), None).await.unwrap();
    assert_eq!(result.images.len(), 1);
}
#[tokio::test]
async fn upstream_processes_bmp_images_from_disk_as_png_attachments() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("test.bmp"), tiny_bmp_1x1_red_24bpp()).unwrap();
    let result = process_file_arguments(&["test.bmp".to_owned()], directory.path(), None).await.unwrap();
    assert_eq!(result.images.len(), 1);
    assert_eq!(result.images[0].mime_type, "image/png");
    assert!(result.text.contains("[Image converted from image/bmp to image/png.]"), "{}", result.text);
}
#[tokio::test]
async fn upstream_processes_text_files_normally() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("test.txt"), "Hello, world!").unwrap();
    let result = process_file_arguments(&["test.txt".to_owned()], directory.path(), None).await.unwrap();
    assert!(result.images.is_empty());
    assert!(result.text.contains("Hello, world!"));
}
#[tokio::test]
async fn upstream_can_defer_resizing_file_attachments_until_prompt_dispatch() {
    let directory = tempfile::tempdir().unwrap();
    let png = tiny_png();
    std::fs::write(directory.path().join("test.png"), &png).unwrap();
    let result = process_file_arguments(&["test.png".to_owned()], directory.path(), Some(false)).await.unwrap();
    assert_eq!(result.images.len(), 1);
    use base64::Engine;
    assert_eq!(result.images[0].data, base64::engine::general_purpose::STANDARD.encode(&png), "the bytes are attached unresized");
}
