//! Port of `packages/coding-agent/src/extensions/html-render/images.ts`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use base64::Engine;

use super::image_references::{find_local_images, image_mime_type_for_path};

const MIB: usize = 1024 * 1024;
pub const MAX_IMAGE_BYTES: usize = 10 * MIB;
pub const MAX_PAGE_BYTES: usize = 25 * MIB;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HtmlRenderImagesError {
    NotFound { paths: Vec<String> },
    ImageTooLarge { path: String, size_bytes: usize },
    PageTooLarge { size_bytes: usize },
}

impl fmt::Display for HtmlRenderImagesError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound { paths } => write!(
                formatter,
                "These local images could not be read: {}. Use absolute paths to existing image files, or remove them.",
                paths.join(", ")
            ),
            Self::ImageTooLarge { path, size_bytes } => write!(
                formatter,
                "{path} is {:.1} MiB; each local image must be at most 10 MiB.",
                *size_bytes as f64 / MIB as f64
            ),
            Self::PageTooLarge { size_bytes } => write!(
                formatter,
                "With its images inlined the page is {:.1} MiB; the limit is 25 MiB. Use smaller images.",
                *size_bytes as f64 / MIB as f64
            ),
        }
    }
}

impl std::error::Error for HtmlRenderImagesError {}

fn file_path_for(reference: &str) -> String {
    let bytes = reference.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        reference.replace("\\\\", "\\")
    } else {
        reference.to_owned()
    }
}

fn data_uri_prefix(path: &str) -> String {
    let mime = image_mime_type_for_path(path).unwrap_or("application/octet-stream");
    format!("data:{mime};base64,")
}

fn after_token(text: &str, token: &str, from: usize) -> Option<usize> {
    text[from..].find(token).map(|p| from + p + token.len())
}

fn after_doctype(text: &str, from: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut in_subset = false;
    let mut at = from;
    while at < bytes.len() {
        let ch = bytes[at];
        if ch == b'"' || ch == b'\'' {
            let token = if ch == b'"' { "\"" } else { "'" };
            at = after_token(text, token, at + 1)?;
        } else if in_subset && bytes[at..].starts_with(b"<!--") {
            at = after_token(text, "-->", at + 4)?;
        } else if in_subset && bytes[at..].starts_with(b"<?") {
            at = after_token(text, "?>", at + 2)?;
        } else if ch == b'>' && !in_subset {
            return Some(at + 1);
        } else {
            if ch == b'[' {
                in_subset = true;
            } else if ch == b']' {
                in_subset = false;
            }
            at += 1;
        }
    }
    None
}

fn has_svg_root(text: &str) -> bool {
    let mut at = 0usize;
    while at < text.len() {
        while at < text.len() && text.as_bytes()[at].is_ascii_whitespace() {
            at += 1;
        }
        if text[at..].starts_with("<?") {
            let Some(next) = after_token(text, "?>", at + 2) else {
                return false;
            };
            at = next;
        } else if text[at..].starts_with("<!--") {
            let Some(next) = after_token(text, "-->", at + 4) else {
                return false;
            };
            at = next;
        } else if text
            .as_bytes()
            .get(at..at + 9)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"<!doctype"))
        {
            let Some(next) = after_doctype(text, at + 9) else {
                return false;
            };
            at = next;
        } else {
            return text[at..].starts_with("<svg")
                && text
                    .as_bytes()
                    .get(at + 4)
                    .is_some_and(|b| matches!(*b, b' ' | b'\t' | b'\r' | b'\n' | b'/' | b'>'));
        }
    }
    false
}

pub fn is_image_bytes(bytes: &[u8]) -> bool {
    if bytes.starts_with(b"\x89PNG")
        || bytes.starts_with(b"\xff\xd8\xff")
        || bytes.starts_with(b"GIF8")
        || bytes.starts_with(b"\x00\x00\x01\x00")
        || (bytes.len() >= 10 && &bytes[0..2] == b"BM" && &bytes[6..10] == b"\x00\x00\x00\x00")
        || (bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP")
        || (bytes.len() >= 12 && matches!(&bytes[4..12], b"ftypavif" | b"ftypavis" | b"ftypmif1"))
    {
        return true;
    }
    let prefix = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]);
    has_svg_root(&prefix)
}

pub struct InlinedHtml {
    pub html: String,
    pub missing: Vec<String>,
}

pub async fn inline_local_images(html: &str) -> Result<InlinedHtml, HtmlRenderImagesError> {
    let references = find_local_images(html);
    let mut unique_paths = Vec::new();
    let mut seen = BTreeSet::new();
    for reference in &references {
        if seen.insert(reference.path.clone()) {
            unique_paths.push(reference.path.clone());
        }
    }
    let mut files = Vec::with_capacity(unique_paths.len());
    for path in &unique_paths {
        let resolved = file_path_for(path);
        let size = match tokio::fs::metadata(&resolved).await {
            Ok(info) if info.is_file() => usize::try_from(info.len()).ok(),
            _ => None,
        };
        if let Some(size) = size
            && size > MAX_IMAGE_BYTES
        {
            return Err(HtmlRenderImagesError::ImageTooLarge {
                path: path.clone(),
                size_bytes: size,
            });
        }
        files.push((path.clone(), size));
    }
    let mut images: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut read_bytes = 0usize;
    for (path, size) in &files {
        if size.is_none() {
            continue;
        }
        let Ok(bytes) = tokio::fs::read(file_path_for(path)).await else {
            continue;
        };
        if !is_image_bytes(&bytes) {
            continue;
        }
        if bytes.len() > MAX_IMAGE_BYTES {
            return Err(HtmlRenderImagesError::ImageTooLarge {
                path: path.clone(),
                size_bytes: bytes.len(),
            });
        }
        read_bytes += bytes.len().div_ceil(3) * 4;
        if read_bytes > MAX_PAGE_BYTES {
            return Err(HtmlRenderImagesError::PageTooLarge {
                size_bytes: read_bytes,
            });
        }
        images.insert(path.clone(), bytes);
    }
    let mut out = String::with_capacity(html.len());
    let mut cursor = 0usize;
    for reference in &references {
        let Some(bytes) = images.get(&reference.path) else {
            continue;
        };
        out.push_str(&html[cursor..reference.start]);
        out.push_str(&data_uri_prefix(&reference.path));
        out.push_str(&base64::engine::general_purpose::STANDARD.encode(bytes));
        cursor = reference.end;
    }
    out.push_str(&html[cursor..]);
    if out.len() > MAX_PAGE_BYTES {
        return Err(HtmlRenderImagesError::PageTooLarge {
            size_bytes: out.len(),
        });
    }
    let missing = files
        .into_iter()
        .filter(|(path, _)| !images.contains_key(path))
        .map(|(path, _)| path)
        .collect();
    Ok(InlinedHtml { html: out, missing })
}

#[cfg(test)]
mod tests {
    use super::is_image_bytes;

    #[test]
    fn svg_doctype_allows_non_ascii_internal_subset() {
        assert!(is_image_bytes("<!DOCTYPE svg [é]><svg/>".as_bytes()));
    }

    #[test]
    fn non_ascii_bytes_across_doctype_prefix_are_not_an_image() {
        assert!(!is_image_bytes("abcdefg한".as_bytes()));
    }
}
