use std::sync::LazyLock;
use regex::Regex;
pub const MAX_IMAGE_BYTES:usize=10*1024*1024;
pub const MAX_TOTAL_BYTES:usize=25*1024*1024;
static ATTACHMENT:LazyLock<Regex>=LazyLock::new(||Regex::new(r"(?i)^[\x09-\x0d\x20\x{a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]*(?:\[?Image #([1-9][0-9]*)(?:,[^\]\n]*)?\]?|(?:attachment|image)://([1-9][0-9]*))[\x09-\x0d\x20\x{a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]*$").expect("literal pattern"));
static DATA_URI:LazyLock<Regex>=LazyLock::new(||Regex::new(r"(?is)^data:([^;,]+)(?:;[^,]*)?,(.*)$").expect("literal pattern"));
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct LoadedLookAtInput { pub data:String,pub label:String,pub mime_type:String }
pub struct LookAtImageInputContext<'a> { pub cwd:&'a std::path::Path,pub branch:&'a [serde_json::Value],pub auto_resize:bool,pub block_images:bool }
fn finalize_input(bytes:Vec<u8>,label:String,mime_type:Option<String>,auto_resize:bool)->Result<(LoadedLookAtInput,usize),String> {
    use base64::Engine;
    let mime_type=input_mime_type(&bytes,&label,mime_type.as_deref())?.to_owned();
    if mime_type.starts_with("image/") && auto_resize { return Err("Image processing requires the source-equivalent processImage binding".into()); }
    let size=bytes.len(); Ok((LoadedLookAtInput{data:base64::engine::general_purpose::STANDARD.encode(bytes),label,mime_type},size))
}
async fn load_path_input(ctx:&LookAtImageInputContext<'_>,input:&str)->Result<(LoadedLookAtInput,usize),String> {
    if input.get(..7).is_some_and(|prefix|prefix.eq_ignore_ascii_case("http://")) || input.get(..8).is_some_and(|prefix|prefix.eq_ignore_ascii_case("https://")) { return Err("Error: Remote URLs are not supported; download first, use local path.".into()); }
    if let Some(reference)=parse_image_attachment_reference(input) {
        let images=last_user_images(ctx.branch);
        let image=if reference.index.is_finite() && reference.index<=images.len() as f64 { images.get(reference.index as usize-1) } else { None }.ok_or_else(||available_attachment_error(input,images.len()))?;
        let data=image["data"].as_str().ok_or_else(||"Attachment data is not a string".to_owned())?;
        let mime=image["mimeType"].as_str().ok_or_else(||"Attachment MIME type is not a string".to_owned())?;
        return finalize_input(decode_base64(data)?,format!("Image #{}",reference.index),Some(mime.to_lowercase()),ctx.auto_resize);
    }
    let input_path=std::path::Path::new(input);
    let path=if input_path.is_absolute() { input_path.to_path_buf() } else {
        let base=if ctx.cwd.is_absolute() { ctx.cwd.to_path_buf() } else { std::env::current_dir().map_err(|error|error.to_string())?.join(ctx.cwd) };
        let mut resolved=std::path::PathBuf::new();
        for part in base.join(input_path).components() { match part { std::path::Component::CurDir=>{},std::path::Component::ParentDir=>{resolved.pop();},other=>resolved.push(other.as_os_str()) } } resolved
    };
    let bytes=tokio::fs::read(&path).await.map_err(|error|if error.kind()==std::io::ErrorKind::NotFound { format!("Error: File not found: {input}") } else { error.to_string() })?;
    let label=path.file_name().unwrap_or_else(||std::ffi::OsStr::new("")).to_string_lossy().into_owned();
    finalize_input(bytes,label,mime_type_from_name(&path.to_string_lossy()).map(String::from),ctx.auto_resize)
}
pub async fn load_look_at_inputs(ctx:&LookAtImageInputContext<'_>,paths:&[String],base64_inputs:&[String])->Result<Vec<LoadedLookAtInput>,String> {
    if ctx.block_images { return Err("Error: Image inputs are blocked by settings.".into()); }
    let paths=paths.iter().map(|path|load_path_input(ctx,path));
    let data=base64_inputs.iter().map(|input|async move { let (data,mime)=parse_base64(input); finalize_input(decode_base64(&data)?,"base64 input".into(),mime,ctx.auto_resize) });
    let (paths,data)=futures::try_join!(futures::future::try_join_all(paths),futures::future::try_join_all(data))?;
    let loaded:Vec<_>=paths.into_iter().chain(data).collect(); validate_aggregate_bytes(&loaded.iter().map(|(_,size)|*size).collect::<Vec<_>>())?;
    Ok(loaded.into_iter().map(|(input,_)|input).collect())
}
#[derive(Clone,Copy,Debug,PartialEq)]
pub struct AttachmentReference { pub index:f64 }
pub fn last_user_images(branch:&[serde_json::Value])->Vec<serde_json::Value> {
    for entry in branch.iter().rev() {
        if entry.get("type").and_then(serde_json::Value::as_str)!=Some("message") { continue; }
        let Some(message)=entry.get("message") else { continue; };
        if message.get("role").and_then(serde_json::Value::as_str)!=Some("user") { continue; }
        return message.get("content").and_then(serde_json::Value::as_array).map(|blocks|blocks.iter().filter(|block|block.get("type").and_then(serde_json::Value::as_str)==Some("image")).cloned().collect()).unwrap_or_default();
    }
    vec![]
}
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
pub fn decode_base64(input:&str)->Result<Vec<u8>,String> {
    let mut data=Vec::new(); let mut bits=0u32; let mut count=0;
    for byte in input.bytes() {
        if byte==b'=' { break; }
        let value=match byte { b'A'..=b'Z'=>byte-b'A',b'a'..=b'z'=>byte-b'a'+26,b'0'..=b'9'=>byte-b'0'+52,b'+'|b'-'=>62,b'/'|b'_'=>63,_=>continue };
        bits=(bits<<6)|u32::from(value); count+=6;
        if count>=8 { count-=8; data.push((bits>>count) as u8); bits&=(1<<count)-1; }
    }
    if data.is_empty() && !input.trim_matches(|c:char|matches!(c,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}')).is_empty() { return Err("Error: Could not decode base64 input.".into()); }
    Ok(data)
}
pub fn input_mime_type<'a>(bytes:&[u8],label:&str,supplied:Option<&'a str>)->Result<&'a str,String> {
    if bytes.len()>MAX_IMAGE_BYTES { return Err("Error: Input exceeds the 10MiB per-image limit.".into()); }
    detect_mime_type(bytes).or(supplied).ok_or_else(||format!("Error: Could not determine MIME type for {label}."))
}
pub fn validate_aggregate_bytes(lengths:&[usize])->Result<(),String> { if lengths.iter().sum::<usize>()>MAX_TOTAL_BYTES { Err("Error: Inputs exceed the 25MiB aggregate limit.".into()) } else { Ok(()) } }
pub fn available_attachment_error(input:&str,count:usize)->String {
    if count==0 { return format!("Error: No image attachments are available in this turn. \"{input}\" must be a readable file path or attachment URI."); }
    let available=(1..=count).map(|index|format!("Image #{index} -> attachment://{index}")).collect::<Vec<_>>().join(", ");
    format!("Error: Could not resolve image attachment '{input}'. Available image attachments: {available}.")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test] async fn loader_preserves_file_attachment_and_base64_order() {
        let directory=tempfile::tempdir().unwrap(); std::fs::write(directory.path().join("note.txt"),b"notes").unwrap();
        let branch=[serde_json::json!({"type":"message","message":{"role":"user","content":[{"type":"image","data":"R0lGOA==","mimeType":"IMAGE/GIF"}]}})];
        let ctx=LookAtImageInputContext{cwd:directory.path(),branch:&branch,auto_resize:false,block_images:false};
        let inputs=load_look_at_inputs(&ctx,&["note.txt".into(),"attachment://1".into()],&["data:application/pdf;base64,JVBERi0=".into()]).await.unwrap();
        assert_eq!(inputs.iter().map(|input|input.label.as_str()).collect::<Vec<_>>(),["note.txt","Image #1","base64 input"]);
        assert_eq!(inputs.iter().map(|input|input.mime_type.as_str()).collect::<Vec<_>>(),["text/plain","image/gif","application/pdf"]); assert_eq!(inputs[0].data,"bm90ZXM=");
    }
    #[tokio::test] async fn loader_rejects_blocked_remote_missing_and_unbound_resize() {
        let directory=tempfile::tempdir().unwrap(); let mut ctx=LookAtImageInputContext{cwd:directory.path(),branch:&[],auto_resize:false,block_images:true};
        assert!(load_look_at_inputs(&ctx,&[],&[]).await.is_err()); ctx.block_images=false;
        assert!(load_look_at_inputs(&ctx,&["https://example.com/a.png".into()],&[]).await.is_err());
        assert_eq!(load_look_at_inputs(&ctx,&["missing.txt".into()],&[]).await.unwrap_err(),"Error: File not found: missing.txt");
        ctx.auto_resize=true; assert!(load_look_at_inputs(&ctx,&[],&["data:image/gif;base64,R0lGOA==".into()]).await.is_err());
    }
    #[test] fn attachment_whitespace_matches_ecmascript() { assert!(parse_image_attachment_reference("\u{feff}Image #1\u{feff}").is_some()); assert!(parse_image_attachment_reference("\u{0085}Image #1").is_none()); }
    #[test] fn latest_user_turn_without_images_does_not_reuse_old_attachments() {
        let old=serde_json::json!({"type":"message","message":{"role":"user","content":[{"type":"image","data":"AA==","mimeType":"image/png"}]}});
        assert_eq!(last_user_images(std::slice::from_ref(&old)).len(),1);
        assert!(last_user_images(&[old,serde_json::json!({"type":"message","message":{"role":"user","content":"text only"}}),serde_json::json!({"type":"message","message":{"role":"assistant","content":[]}})]).is_empty());
    }
    #[test] fn base64_accepts_node_whitespace_urlsafe_and_missing_padding() { assert_eq!(decode_base64(" aG Vs bG8 ").unwrap(),b"hello"); assert_eq!(decode_base64("-_8").unwrap(),[251,255]); assert_eq!(decode_base64("aGk=ignored").unwrap(),b"hi"); assert!(decode_base64("???").is_err()); assert!(decode_base64(" \u{feff}").unwrap().is_empty()); }
    #[test] fn signature_precedes_supplied_mime_and_limit_is_inclusive() { assert_eq!(input_mime_type(b"%PDF-1.7","file",Some("image/png")).unwrap(),"application/pdf"); assert!(input_mime_type(&vec![1;MAX_IMAGE_BYTES],"file",Some("image/png")).is_ok()); assert!(input_mime_type(&vec![1;MAX_IMAGE_BYTES+1],"file",Some("image/png")).is_err()); assert!(input_mime_type(b"unknown","file",None).is_err()); }
    #[test] fn aggregate_limit_is_inclusive() { assert!(validate_aggregate_bytes(&[MAX_TOTAL_BYTES]).is_ok()); assert!(validate_aggregate_bytes(&[MAX_TOTAL_BYTES,1]).is_err()); }
    #[test] fn attachment_reference_forms() { for value in ["Image #2"," [Image #2, size: 10] ","ATTACHMENT://2","image://2"] { assert_eq!(parse_image_attachment_reference(value),Some(AttachmentReference{index:2.})); } for value in ["Image #0","attachment://01","Image #2\ntext"] { assert_eq!(parse_image_attachment_reference(value),None); } }
    #[test] fn mime_signatures_and_extensions() { assert_eq!(detect_mime_type(b"%PDF-1.7"),Some("application/pdf")); assert_eq!(detect_mime_type(b"RIFF1234WEBP"),Some("image/webp")); assert_eq!(detect_mime_type(b"RIFF"),None); assert_eq!(mime_type_from_name("IMAGE.JPEG"),Some("image/jpeg")); assert_eq!(mime_type_from_name("unknown"),None); }
    #[test] fn data_uri_preserves_multiline_payload() { assert_eq!(parse_base64("DATA:IMAGE/PNG;charset=utf8;base64,abc\ndef"),("abc\ndef".into(),Some("image/png".into()))); assert_eq!(parse_base64("abc"),("abc".into(),None)); }
}
