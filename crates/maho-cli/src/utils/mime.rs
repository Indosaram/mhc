pub use maho_tools::read::detect_supported_image_mime_type;
pub fn detect_supported_image_mime_type_from_file(path: &std::path::Path) -> std::io::Result<Option<&'static str>> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?; let mut buffer = [0; 4100]; let count = file.read(&mut buffer)?; Ok(detect_supported_image_mime_type(&buffer[..count]))
}
