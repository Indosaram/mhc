use maho_cli::utils::image_convert::*;
use base64::Engine;
#[test]
fn png_passthrough_does_not_decode() {
    let result = convert_to_png("not base64", "image/png").unwrap();
    assert_eq!(result.data, "not base64");
    assert_eq!(result.mime_type, "image/png");
}
#[test]
fn jpeg_converts_to_decodable_png_with_original_dimensions() {
    let jpeg = photon_rs::PhotonImage::new(vec![255; 12 * 6 * 4], 12, 6).get_bytes_jpeg(80);
    let result = convert_to_png(&base64::engine::general_purpose::STANDARD.encode(jpeg), "image/jpeg").unwrap();
    let png = base64::engine::general_purpose::STANDARD.decode(result.data).unwrap();
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    let image = photon_rs::PhotonImage::new_from_byteslice(png);
    assert_eq!((image.get_width(), image.get_height()), (12, 6));
}
#[test]
fn invalid_image_bytes_fail_conversion() {
    assert!(convert_image_bytes_to_png(b"invalid").is_none());
}
