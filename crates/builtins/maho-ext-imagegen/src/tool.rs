use std::{fs::{self,OpenOptions},io::Write,path::{Path,PathBuf}};

pub const GENERATE_IMAGE_TOOL_NAME: &str = "generate_image";
pub struct GeneratedImage { pub data: String, pub mime_type: String, pub revised_prompt: Option<String> }
fn decode_base64(data: &str) -> Vec<u8> {
    let mut bytes=Vec::new();let mut bits=0u32;let mut count=0;
    for byte in data.bytes() {
        let value=match byte {b'A'..=b'Z'=>byte-b'A',b'a'..=b'z'=>byte-b'a'+26,b'0'..=b'9'=>byte-b'0'+52,b'+'|b'-'=>62,b'/'|b'_'=>63,b'='=>break,_=>continue};
        bits=(bits<<6)|u32::from(value);count+=6;
        if count>=8 {count-=8;bytes.push((bits>>count) as u8);}
    }
    bytes
}
pub fn write_images(paths: &[PathBuf], images: &[GeneratedImage]) -> Result<(),String> {
    let mut written: Vec<&Path> = Vec::new();
    for (target,image) in paths.iter().zip(images) {
        let write = || -> Result<(),String> {
            if let Some(parent)=target.parent() { fs::create_dir_all(parent).map_err(|e|e.to_string())?; }
            let mut file=OpenOptions::new().write(true).create_new(true).open(target).map_err(|e|e.to_string())?;
            let bytes=decode_base64(&image.data);
            file.write_all(&bytes).map_err(|e|e.to_string())
        };
        if let Err(reason)=write() {
            for path in written { let _cleanup=fs::remove_file(path); }
            return Err(format!("Error: failed to write generated image to {}: {reason}",target.display()));
        }
        written.push(target);
    }
    Ok(())
}
