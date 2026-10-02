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
