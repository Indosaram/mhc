use std::path::{Component, Path, PathBuf};

pub const GENERATED_IMAGE_DIRECTORY: &str = "generated-images";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputFormat { Png, Jpeg, Webp }
impl OutputFormat {
    fn extensions(self) -> &'static [&'static str] {
        match self { Self::Png => &[".png"], Self::Jpeg => &[".jpg", ".jpeg"], Self::Webp => &[".webp"] }
    }
    fn name(self) -> &'static str {
        match self { Self::Png => "png", Self::Jpeg => "jpeg", Self::Webp => "webp" }
    }
}
pub fn sanitize_image_stem(identifier: &str) -> String {
    let stem: String = identifier.encode_utf16().take(64).map(|unit| {
        if unit <= 127 && ((unit as u8).is_ascii_alphanumeric() || unit == 45 || unit == 95) {
            char::from(unit as u8)
        } else { '_' }
    }).collect();
    if stem.is_empty() { "image".into() } else { stem }
}
fn extension_start(path: &str) -> Option<usize> {
    let index = path.rfind('.')?;
    (!path[index + 1..].is_empty() && !path[index + 1..].contains(['/', '\\'])).then_some(index)
}
fn resolve(cwd: &Path, requested: &Path) -> PathBuf {
    let absolute = if requested.is_absolute() { requested.to_path_buf() } else { cwd.join(requested) };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {},
            Component::ParentDir => { normalized.pop(); },
            component => normalized.push(component.as_os_str()),
        }
    }
    normalized
}
pub fn resolve_targets(cwd: &Path, tool_call_id: &str, count: usize, output_path: Option<&str>, format: OutputFormat) -> Result<Vec<String>, String> {
    let allowed = format.extensions();
    let default_extension = allowed[0];
    let requested = output_path.map(str::trim).filter(|path| !path.is_empty());
    let (base, extension) = match requested {
        None => (resolve(cwd, &Path::new(GENERATED_IMAGE_DIRECTORY).join(format!("{}{default_extension}", sanitize_image_stem(tool_call_id)))).to_string_lossy().into_owned(), default_extension.to_owned()),
        Some(requested) => {
            let absolute = resolve(cwd, Path::new(requested)).to_string_lossy().into_owned();
            match extension_start(&absolute) {
                Some(index) => {
                    let extension = &absolute[index..];
                    if !allowed.contains(&extension.to_ascii_lowercase().as_str()) {
                        return Err(format!("Error: output_path must end in {} for output_format {} (got \"{requested}\").", allowed.join(" or "), format.name()));
                    }
                    (absolute.clone(), extension.to_owned())
                }
                None => (format!("{absolute}{default_extension}"), default_extension.to_owned()),
            }
        }
    };
    if count == 1 { return Ok(vec![base]); }
    let stem = &base[..base.len() - extension.len()];
    Ok((1..=count).map(|index| format!("{stem}-{index:02}{extension}")).collect())
}
pub fn output_format_of(mime_type: &str) -> Option<OutputFormat> {
    match mime_type { "image/png" => Some(OutputFormat::Png), "image/jpeg" => Some(OutputFormat::Jpeg), "image/webp" => Some(OutputFormat::Webp), _ => None }
}
pub fn with_format_extension(path: &str, format: OutputFormat) -> String {
    let allowed = format.extensions();
    match extension_start(path) {
        Some(index) if allowed.contains(&path[index..].to_ascii_lowercase().as_str()) => path.into(),
        Some(index) => format!("{}{}", &path[..index], allowed[0]),
        None => format!("{path}{}", allowed[0]),
    }
}
pub fn display_path(cwd: &Path, absolute: &Path) -> String {
    match absolute.strip_prefix(cwd) {
        Ok(relative) if !relative.as_os_str().is_empty() && !relative.to_string_lossy().starts_with("..") => relative.to_string_lossy().into_owned(),
        _ => absolute.to_string_lossy().into_owned(),
    }
}
