use maho_ext_imagegen::paths::*;
use std::path::Path;

#[test]
fn defaults_sanitize_utf16_and_number_multiple_images() {
    let paths = resolve_targets(Path::new("/work"), "../a😀", 2, None, OutputFormat::Png).expect("targets");
    assert_eq!(paths, vec!["/work/generated-images/___a__-01.png", "/work/generated-images/___a__-02.png"]);
}
#[test]
fn incompatible_extension_rejects_before_generation() {
    assert!(resolve_targets(Path::new("/work"), "id", 1, Some("photo.png"), OutputFormat::Jpeg).is_err());
}
#[test]
fn explicit_targets_normalize_and_preserve_supported_extension_case() {
    assert_eq!(resolve_targets(Path::new("/work"), "id", 1, Some("dir/../photo.JPEG"), OutputFormat::Jpeg).expect("targets"), vec!["/work/photo.JPEG"]);
}
#[test]
fn returned_mime_controls_extension_without_changing_jpeg_alias() {
    assert_eq!(output_format_of("image/jpeg"), Some(OutputFormat::Jpeg));
    assert_eq!(output_format_of("image/gif"), None);
    assert_eq!(with_format_extension("photo.jpeg", OutputFormat::Jpeg), "photo.jpeg");
    assert_eq!(with_format_extension("photo.png", OutputFormat::Webp), "photo.webp");
}
#[test]
fn display_uses_relative_only_for_nonempty_inside_paths() {
    assert_eq!(display_path(Path::new("/work"), Path::new("/work/photo.png")), "photo.png");
    assert_eq!(display_path(Path::new("/work"), Path::new("/other/photo.png")), "/other/photo.png");
    assert_eq!(display_path(Path::new("/work"), Path::new("/work")), "/work");
}
