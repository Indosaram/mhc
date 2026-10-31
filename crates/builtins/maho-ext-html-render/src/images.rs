//! Local-image preparation, mirroring the desktop's apps/server/src/htmlRender/HtmlRender.ts
//! (upstream t3code #15968): absolute-path images are inlined as data URIs only after a magic-byte/SVG-root
//! check, so a renamed secret is refused, and the page stays within the size caps.
use base64::Engine;
use fancy_regex::Regex;
use std::{collections::HashMap, sync::OnceLock};

const MIB: u64 = 1024 * 1024;
const MAX_IMAGE_BYTES: u64 = 10 * MIB;
const MAX_PAGE_BYTES: u64 = 25 * MIB;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HtmlRenderImagesError {
    NotFound(Vec<String>),
    ImageTooLarge { path: String, size_bytes: u64 },
    PageTooLarge { size_bytes: u64 },
}

impl std::fmt::Display for HtmlRenderImagesError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(paths) => write!(
                formatter,
                "These local images could not be read: {}. Use absolute paths to existing image files, or remove them.",
                paths.join(", ")
            ),
            Self::ImageTooLarge { path, size_bytes } => {
                write!(
                    formatter,
                    "{path} is {:.1} MiB; each local image must be at most 10 MiB.",
                    *size_bytes as f64 / MIB as f64
                )
            }
            Self::PageTooLarge { size_bytes } => write!(
                formatter,
                "With its images inlined the page is {:.1} MiB; the limit is 25 MiB. Use smaller images.",
                *size_bytes as f64 / MIB as f64
            ),
        }
    }
}

impl std::error::Error for HtmlRenderImagesError {}

const IMAGE_MIME_TYPES: [(&str, &str); 9] = [
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("avif", "image/avif"),
    ("svg", "image/svg+xml"),
    ("bmp", "image/bmp"),
    ("ico", "image/x-icon"),
];

fn local_image_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        let extensions = IMAGE_MIME_TYPES.map(|(extension, _)| extension).join("|");
        let absolute = r"(?:/(?!/)|[a-z]:[\\/])";
        Regex::new(&format!(
            r#"(?i)(["'\x60])({absolute}(?:(?!\1)[^\r\n]){{0,2048}}?\.(?:{extensions}))\1|url\(\s*({absolute}[^\s"'\x60()]{{0,2048}}?\.(?:{extensions}))\s*\)"#
        ))
        .unwrap_or_else(|error| panic!("invalid local image pattern: {error}"))
    })
}

struct ImageReference {
    start: usize,
    end: usize,
    path: String,
}

fn find_local_images(html: &str) -> Vec<ImageReference> {
    local_image_pattern()
        .captures_iter(html)
        .flatten()
        .filter_map(|captures| {
            let span = captures.get(2).or_else(|| captures.get(3))?;
            Some(ImageReference {
                start: span.start(),
                end: span.end(),
                path: span.as_str().to_owned(),
            })
        })
        .collect()
}

fn file_path_for(reference: &str) -> String {
    let mut bytes = reference.bytes();
    let drive =
        bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic()) && bytes.next() == Some(b':');
    if drive {
        reference.replace("\\\\", "\\")
    } else {
        reference.to_owned()
    }
}

fn data_uri_prefix(path: &str) -> String {
    let extension = path
        .rsplit_once('.')
        .map_or(path, |(_, extension)| extension)
        .to_lowercase();
    let mime = IMAGE_MIME_TYPES
        .iter()
        .find(|(key, _)| *key == extension)
        .map_or("application/octet-stream", |(_, mime)| *mime);
    format!("data:{mime};base64,")
}

fn latin1(bytes: &[u8], start: usize, end: usize) -> String {
    bytes
        .get(start..end.min(bytes.len()))
        .unwrap_or(&[])
        .iter()
        .map(|byte| char::from(*byte))
        .collect()
}

fn after(text: &[char], token: &str, from: usize) -> Option<usize> {
    let token = token.chars().collect::<Vec<_>>();
    (from..text.len())
        .find(|at| text[*at..].starts_with(&token))
        .map(|at| at + token.len())
}

fn starts_with_at(text: &[char], at: usize, token: &str) -> bool {
    text.get(at..).is_some_and(|rest| {
        token.chars().zip(rest).all(|(left, right)| left == *right)
            && rest.len() >= token.chars().count()
    })
}

fn after_doctype(text: &[char], from: usize) -> Option<usize> {
    let mut in_subset = false;
    let mut at = Some(from);
    while let Some(index) = at.filter(|index| *index < text.len()) {
        let char = text[index];
        if char == '"' || char == '\'' {
            at = after(text, &char.to_string(), index + 1);
        } else if in_subset && starts_with_at(text, index, "<!--") {
            at = after(text, "-->", index + 4);
        } else if in_subset && starts_with_at(text, index, "<?") {
            at = after(text, "?>", index + 2);
        } else if char == '>' && !in_subset {
            return Some(index + 1);
        } else {
            if char == '[' {
                in_subset = true;
            } else if char == ']' {
                in_subset = false;
            }
            at = Some(index + 1);
        }
    }
    None
}

fn has_svg_root(text: &[char]) -> bool {
    let mut at = Some(0);
    while let Some(mut index) = at {
        while text.get(index).is_some_and(|char| char.is_whitespace()) {
            index += 1;
        }
        if starts_with_at(text, index, "<?") {
            at = after(text, "?>", index + 2);
        } else if starts_with_at(text, index, "<!--") {
            at = after(text, "-->", index + 4);
        } else if text
            .get(index..index + 9)
            .is_some_and(|head| head.iter().collect::<String>().to_lowercase() == "<!doctype")
        {
            at = after_doctype(text, index + 9);
        } else {
            let head = text.get(index..(index + 5).min(text.len())).unwrap_or(&[]);
            return head.len() == 5
                && head[..4] == ['<', 's', 'v', 'g']
                && matches!(head[4], ' ' | '\t' | '\r' | '\n' | '/' | '>');
        }
    }
    false
}

/// Whether file bytes are an image, whatever the file is named.
fn is_image_bytes(bytes: &[u8]) -> bool {
    let head = latin1(bytes, 0, 12);
    let tail = |start: usize, end: usize| {
        head.chars()
            .skip(start)
            .take(end - start)
            .collect::<String>()
    };
    if head.starts_with("\u{89}PNG")
        || head.starts_with("\u{ff}\u{d8}\u{ff}")
        || head.starts_with("GIF8")
        || head.starts_with("\0\0\u{1}\0")
        || (head.starts_with("BM") && tail(6, 10) == "\0\0\0\0")
        || (head.starts_with("RIFF") && tail(8, 12) == "WEBP")
        || ["ftypavif", "ftypavis", "ftypmif1"].contains(&tail(4, 12).as_str())
    {
        return true;
    }
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)])
        .chars()
        .collect::<Vec<_>>();
    has_svg_root(&text)
}

/// Replaces every local image reference with a data URI. Returns the page and the paths that were left as
/// written (unreadable, not a file, or not an image); oversized images fail.
pub async fn inline_local_images(
    html: &str,
) -> Result<(String, Vec<String>), HtmlRenderImagesError> {
    let references = find_local_images(html);
    let mut unique = Vec::<String>::new();
    for reference in &references {
        if !unique.contains(&reference.path) {
            unique.push(reference.path.clone());
        }
    }
    let mut files = Vec::new();
    for path in unique {
        let size = match tokio::fs::metadata(file_path_for(&path)).await {
            Ok(info) if info.is_file() => Some(info.len()),
            _ => None,
        };
        files.push((path, size));
    }
    if let Some((path, Some(size))) = files
        .iter()
        .find(|(_, size)| size.is_some_and(|size| size > MAX_IMAGE_BYTES))
    {
        return Err(HtmlRenderImagesError::ImageTooLarge {
            path: path.clone(),
            size_bytes: *size,
        });
    }
    let mut images = HashMap::<String, Vec<u8>>::new();
    let mut read_bytes = 0u64;
    for (path, _) in files.iter().filter(|(_, size)| size.is_some()) {
        let Ok(bytes) = tokio::fs::read(file_path_for(path)).await else {
            continue;
        };
        if !is_image_bytes(&bytes) {
            continue;
        }
        if bytes.len() as u64 > MAX_IMAGE_BYTES {
            return Err(HtmlRenderImagesError::ImageTooLarge {
                path: path.clone(),
                size_bytes: bytes.len() as u64,
            });
        }
        read_bytes += (bytes.len() as u64).div_ceil(3) * 4;
        if read_bytes > MAX_PAGE_BYTES {
            return Err(HtmlRenderImagesError::PageTooLarge {
                size_bytes: read_bytes,
            });
        }
        images.insert(path.clone(), bytes);
    }
    let mut inlined = String::with_capacity(html.len());
    let mut cursor = 0;
    for reference in &references {
        let Some(bytes) = images.get(&reference.path) else {
            continue;
        };
        inlined.push_str(&html[cursor..reference.start]);
        inlined.push_str(&data_uri_prefix(&reference.path));
        inlined.push_str(&base64::engine::general_purpose::STANDARD.encode(bytes));
        cursor = reference.end;
    }
    inlined.push_str(&html[cursor..]);
    if inlined.len() as u64 > MAX_PAGE_BYTES {
        return Err(HtmlRenderImagesError::PageTooLarge {
            size_bytes: inlined.len() as u64,
        });
    }
    let missing = files
        .into_iter()
        .filter(|(path, _)| !images.contains_key(path))
        .map(|(path, _)| path)
        .collect();
    Ok((inlined, missing))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

    #[tokio::test]
    async fn absolute_png_is_inlined_and_a_renamed_secret_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let png = dir.path().join("shot.png");
        let secret = dir.path().join("secret.png");
        std::fs::write(&png, PNG).expect("png");
        std::fs::write(&secret, b"password=hunter2").expect("secret");
        let html = format!(
            r#"<img src="{}"><img src="{}"><img src="/no/such.png">"#,
            png.display(),
            secret.display()
        );
        let (out, missing) = inline_local_images(&html).await.expect("inlined");
        assert!(out.contains("src=\"data:image/png;base64,"));
        assert!(
            out.contains(&secret.display().to_string()),
            "a non-image keeps its path"
        );
        assert_eq!(
            missing,
            vec![secret.display().to_string(), "/no/such.png".to_owned()]
        );
    }

    #[test]
    fn svg_roots_are_found_after_prolog_comments_and_doctype() {
        assert!(is_image_bytes(b"<?xml version=\"1.0\"?><!-- c --><!DOCTYPE svg [<!ENTITY a \">\">]><svg xmlns=\"x\"/>"));
        assert!(!is_image_bytes(b"<html><svg></svg></html>"));
    }

    #[test]
    fn local_image_references_cover_quotes_and_url_functions() {
        let found = find_local_images(
            r#"<img src="/a/b.PNG"> body{background:url( /c/d.svg )} "//cdn/x.png""#,
        );
        assert_eq!(
            found
                .iter()
                .map(|reference| reference.path.as_str())
                .collect::<Vec<_>>(),
            vec!["/a/b.PNG", "/c/d.svg"]
        );
    }
}
