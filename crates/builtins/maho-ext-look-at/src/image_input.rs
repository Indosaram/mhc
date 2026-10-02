use std::sync::LazyLock;
use regex::Regex;
pub const MAX_IMAGE_BYTES:usize=10*1024*1024;
pub const MAX_TOTAL_BYTES:usize=25*1024*1024;
static ATTACHMENT:LazyLock<Regex>=LazyLock::new(||Regex::new(r"(?i)^\s*(?:\[?Image #([1-9][0-9]*)(?:,[^\]\n]*)?\]?|(?:attachment|image)://([1-9][0-9]*))\s*$").expect("literal pattern"));
static DATA_URI:LazyLock<Regex>=LazyLock::new(||Regex::new(r"(?is)^data:([^;,]+)(?:;[^,]*)?,(.*)$").expect("literal pattern"));
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct LoadedLookAtInput { pub data:String,pub label:String,pub mime_type:String }
#[derive(Clone,Copy,Debug,PartialEq)]
pub struct AttachmentReference { pub index:f64 }
pub fn parse_image_attachment_reference(input:&str)->Option<AttachmentReference> { let capture=ATTACHMENT.captures(input)?; let raw=capture.get(1).or_else(||capture.get(2))?; Some(AttachmentReference{index:raw.as_str().parse().unwrap_or(f64::INFINITY)}) }
pub fn detect_mime_type(bytes:&[u8])->Option<&'static str> {
    if bytes.starts_with(&[0x89,0x50,0x4e,0x47,0x0d,0x0a,0x1a,0x0a]) { Some("image/png") }
    else if bytes.starts_with(&[0xff,0xd8,0xff]) { Some("image/jpeg") }
    else if bytes.starts_with(b"GIF8") { Some("image/gif") }
    else if bytes.starts_with(b"RIFF") && bytes.get(8..12)==Some(b"WEBP") { Some("image/webp") }
    else if bytes.starts_with(b"%PDF-") { Some("application/pdf") } else { None }
}
pub fn mime_type_from_name(name:&str)->Option<&'static str> { match std::path::Path::new(name).extension()?.to_str()?.to_lowercase().as_str() { "gif"=>Some("image/gif"),"jpeg"|"jpg"=>Some("image/jpeg"),"json"=>Some("application/json"),"pdf"=>Some("application/pdf"),"png"=>Some("image/png"),"txt"=>Some("text/plain"),"webp"=>Some("image/webp"),_=>None } }
pub fn parse_base64(input:&str)->(String,Option<String>) { match DATA_URI.captures(input) { Some(capture)=>(capture[2].into(),Some(capture[1].to_lowercase())),None=>(input.into(),None) } }
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn attachment_reference_forms() { for value in ["Image #2"," [Image #2, size: 10] ","ATTACHMENT://2","image://2"] { assert_eq!(parse_image_attachment_reference(value),Some(AttachmentReference{index:2.})); } for value in ["Image #0","attachment://01","Image #2\ntext"] { assert_eq!(parse_image_attachment_reference(value),None); } }
    #[test] fn mime_signatures_and_extensions() { assert_eq!(detect_mime_type(b"%PDF-1.7"),Some("application/pdf")); assert_eq!(detect_mime_type(b"RIFF1234WEBP"),Some("image/webp")); assert_eq!(detect_mime_type(b"RIFF"),None); assert_eq!(mime_type_from_name("IMAGE.JPEG"),Some("image/jpeg")); assert_eq!(mime_type_from_name("unknown"),None); }
    #[test] fn data_uri_preserves_multiline_payload() { assert_eq!(parse_base64("DATA:IMAGE/PNG;charset=utf8;base64,abc\ndef"),("abc\ndef".into(),Some("image/png".into()))); assert_eq!(parse_base64("abc"),("abc".into(),None)); }
}
