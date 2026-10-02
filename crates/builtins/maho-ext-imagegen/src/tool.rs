use std::{fs::{self,OpenOptions},io::Write,path::{Path,PathBuf}};
use base64::{Engine,engine::general_purpose::STANDARD};

pub const GENERATE_IMAGE_TOOL_NAME: &str = "generate_image";
pub struct GeneratedImage { pub data: String, pub mime_type: String, pub revised_prompt: Option<String> }
pub fn write_images(paths: &[PathBuf], images: &[GeneratedImage]) -> Result<(),String> {
    let mut written: Vec<&Path> = Vec::new();
    for (target,image) in paths.iter().zip(images) {
        let write = || -> Result<(),String> {
            if let Some(parent)=target.parent() { fs::create_dir_all(parent).map_err(|e|e.to_string())?; }
            let mut file=OpenOptions::new().write(true).create_new(true).open(target).map_err(|e|e.to_string())?;
            let bytes=STANDARD.decode(&image.data).map_err(|e|e.to_string())?;
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
