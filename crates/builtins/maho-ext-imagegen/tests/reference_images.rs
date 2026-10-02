use maho_ext_imagegen::reference_images::*;
#[test]
fn references_are_ordered_and_masks_require_a_reference() {
    let cwd = tempfile::tempdir().expect("temp dir");
    std::fs::write(cwd.path().join("jpeg"), [255,216,255]).expect("image");
    std::fs::write(cwd.path().join("webp"), b"RIFFxxxxWEBP").expect("image");
    let images = load_reference_images(cwd.path(), Some(&["jpeg".into(),"webp".into()])).expect("load");
    assert_eq!(images[0].mime_type, "image/jpeg");
    assert_eq!(images[0].data, "/9j/");
    assert_eq!(images[1].mime_type, "image/webp");
    assert!(load_mask_image(cwd.path(), Some("jpeg"), 0).expect("requested").is_err());
    assert!(load_mask_image(cwd.path(), None, 0).is_none());
}
#[test]
fn invalid_cardinality_magic_and_regular_file_are_rejected() {
    let cwd = tempfile::tempdir().expect("temp dir");
    assert!(load_reference_images(cwd.path(), None).expect("none").is_empty());
    assert!(load_reference_images(cwd.path(), Some(&[])).is_err());
    assert!(load_reference_images(cwd.path(), Some(&vec!["x".into();6])).is_err());
    std::fs::write(cwd.path().join("invalid"), b"text").expect("invalid file");
    assert!(load_reference_images(cwd.path(), Some(&["invalid".into()])).is_err());
    assert!(load_reference_images(cwd.path(), Some(&[".".into()])).is_err());
}
