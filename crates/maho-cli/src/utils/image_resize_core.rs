use base64::Engine;
pub struct ImageResizeOptions { pub max_width: u32, pub max_height: u32, pub max_bytes: f64, pub jpeg_quality: u8 }
impl Default for ImageResizeOptions { fn default() -> Self { Self { max_width: 2000, max_height: 2000, max_bytes: 4.5 * 1024.0 * 1024.0, jpeg_quality: 80 } } }
pub struct ResizedImage { pub data: String, pub mime_type: String, pub original_width: u32, pub original_height: u32, pub width: u32, pub height: u32, pub was_resized: bool }
pub fn resize_image_in_process(bytes: &[u8], mime: &str, options: &ImageResizeOptions) -> Option<ResizedImage> {
    std::panic::catch_unwind(|| {
        let raw = photon_rs::PhotonImage::new_from_byteslice(bytes.to_vec());
        let (pixels, width, height) = super::exif_orientation::apply_exif_orientation(&raw.get_raw_pixels(), raw.get_width() as usize, raw.get_height() as usize, bytes);
        let image = photon_rs::PhotonImage::new(pixels, width as u32, height as u32);
        let (original_width, original_height) = (image.get_width(), image.get_height());
        let encode = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
        if original_width <= options.max_width && original_height <= options.max_height && ((bytes.len().div_ceil(3) * 4) as f64) < options.max_bytes {
            return Some(ResizedImage { data: encode(bytes), mime_type: if mime.is_empty() { "image/png".to_owned() } else { mime.to_owned() }, original_width, original_height, width: original_width, height: original_height, was_resized: false });
        }
        let (mut width, mut height) = (original_width, original_height);
        if width > options.max_width { height = ((height as f64 * options.max_width as f64) / width as f64).round() as u32; width = options.max_width; }
        if height > options.max_height { width = ((width as f64 * options.max_height as f64) / height as f64).round() as u32; height = options.max_height; }
        let mut qualities = Vec::new();
        for quality in [options.jpeg_quality, 85, 70, 55, 40] { if !qualities.contains(&quality) { qualities.push(quality); } }
        loop {
            let resized = photon_rs::transform::resize(&image, width, height, photon_rs::transform::SamplingFilter::Lanczos3);
            let mut candidates = vec![(resized.get_bytes(), "image/png")];
            for quality in &qualities { candidates.push((resized.get_bytes_jpeg(*quality), "image/jpeg")); }
            for (bytes, mime) in candidates {
                let data = encode(&bytes);
                if (data.len() as f64) < options.max_bytes { return Some(ResizedImage { data, mime_type: mime.to_owned(), original_width, original_height, width, height, was_resized: true }); }
            }
            if width == 1 && height == 1 { break; }
            let next_width = ((width as f64 * 0.75).floor() as u32).max(1);
            let next_height = ((height as f64 * 0.75).floor() as u32).max(1);
            if next_width == width && next_height == height { break; }
            (width, height) = (next_width, next_height);
        }
        None
    }).ok().flatten()
}
