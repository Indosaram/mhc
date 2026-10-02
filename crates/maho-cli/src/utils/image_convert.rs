use base64::Engine;
pub struct ConvertedImage { pub data: String, pub mime_type: String }
pub fn convert_image_bytes_to_png(bytes: &[u8]) -> Option<Vec<u8>> {
    std::panic::catch_unwind(|| {
        let raw = photon_rs::PhotonImage::new_from_byteslice(bytes.to_vec());
        let (pixels, width, height) = super::exif_orientation::apply_exif_orientation(&raw.get_raw_pixels(), raw.get_width() as usize, raw.get_height() as usize, bytes);
        photon_rs::PhotonImage::new(pixels, width as u32, height as u32).get_bytes()
    }).ok()
}
pub fn convert_to_png(data: &str, mime_type: &str) -> Option<ConvertedImage> {
    if mime_type == "image/png" { return Some(ConvertedImage { data: data.to_owned(), mime_type: mime_type.to_owned() }); }
    // Buffer.from(base64) tolerates whitespace and URL-safe alphabet.
    let normalized: String = data.chars().take_while(|character| *character != '=').filter(|character| character.is_ascii_alphanumeric() || matches!(character, '+' | '/' | '-' | '_')).map(|character| match character { '-' => '+', '_' => '/', character => character }).collect();
    let bytes = base64::engine::general_purpose::STANDARD_NO_PAD.decode(normalized).ok()?;
    let png = convert_image_bytes_to_png(&bytes)?;
    Some(ConvertedImage { data: base64::engine::general_purpose::STANDARD.encode(png), mime_type: "image/png".to_owned() })
}
