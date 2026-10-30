//! Port of `tool/settled-image-spill.ts` (senpi 903b8336): keeps settled-cell image payloads on
//! disk instead of the session heap (#2259). Each image is one raw base64 file; the oldest files go
//! first beyond the byte budget (never the newest cell's), a snapshot's files go with it, and the
//! whole directory goes on clear.

use maho_ai::types::{ContentBlock, ImageContent, TextContent};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct SpilledImageRef {
    pub path: PathBuf,
    pub mime_type: String,
    pub length: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RetainedContentPart {
    Inline(ContentBlock),
    Spilled(SpilledImageRef),
}

pub struct SettledImageSpill {
    dir: PathBuf,
    /// Upper bound on the spilled base64 bytes on disk; 0 keeps only the snapshot count cap.
    byte_budget: u64,
    /// Insertion-ordered files: (path, cell id, bytes).
    files: Vec<(PathBuf, String, u64)>,
    bytes: u64,
    sequence: u64,
}

impl SettledImageSpill {
    pub fn new(dir: PathBuf, byte_budget: u64) -> Self {
        Self {
            dir,
            byte_budget,
            files: Vec::new(),
            bytes: 0,
            sequence: 0,
        }
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn spill(&mut self, cell_id: &str, content: &[ContentBlock]) -> Vec<RetainedContentPart> {
        if !content
            .iter()
            .any(|part| matches!(part, ContentBlock::Image(_)))
        {
            return content
                .iter()
                .cloned()
                .map(RetainedContentPart::Inline)
                .collect();
        }
        // A directory that cannot be created keeps every image inline, like a failed write.
        let _ = std::fs::create_dir_all(&self.dir);
        let retained = content
            .iter()
            .map(|part| match part {
                ContentBlock::Image(image) => self.write(cell_id, image),
                other => RetainedContentPart::Inline(other.clone()),
            })
            .collect();
        self.enforce_budget(cell_id);
        retained
    }

    pub fn hydrate(&self, parts: &[RetainedContentPart]) -> Vec<ContentBlock> {
        parts
            .iter()
            .map(|part| match part {
                RetainedContentPart::Inline(block) => block.clone(),
                RetainedContentPart::Spilled(reference) => read_spilled_image(reference),
            })
            .collect()
    }

    pub fn release(&mut self, parts: &[RetainedContentPart]) {
        for part in parts {
            if let RetainedContentPart::Spilled(reference) = part {
                self.remove(&reference.path);
            }
        }
    }

    pub fn clear(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
        self.files.clear();
        self.bytes = 0;
    }

    fn write(&mut self, cell_id: &str, image: &ImageContent) -> RetainedContentPart {
        self.sequence += 1;
        let safe: String = cell_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let path = self.dir.join(format!("{safe}-{}.b64", self.sequence));
        // A full or read-only disk keeps this image inline, where the in-memory byte budget bounds it.
        if std::fs::write(&path, image.data.as_bytes()).is_err() {
            return RetainedContentPart::Inline(ContentBlock::Image(image.clone()));
        }
        let length = image.data.len();
        self.files
            .push((path.clone(), cell_id.to_owned(), length as u64));
        self.bytes += length as u64;
        RetainedContentPart::Spilled(SpilledImageRef {
            path,
            mime_type: image.mime_type.clone(),
            length,
        })
    }

    fn enforce_budget(&mut self, newest_cell_id: &str) {
        if self.byte_budget == 0 {
            return;
        }
        let candidates: Vec<PathBuf> = self
            .files
            .iter()
            .filter(|(_, cell_id, _)| cell_id != newest_cell_id)
            .map(|(path, _, _)| path.clone())
            .collect();
        for path in candidates {
            if self.bytes <= self.byte_budget {
                return;
            }
            self.remove(&path);
        }
    }

    fn remove(&mut self, path: &Path) {
        let Some(index) = self.files.iter().position(|(file, _, _)| file == path) else {
            return;
        };
        let (path, _, bytes) = self.files.remove(index);
        self.bytes -= bytes;
        let _ = std::fs::remove_file(path);
    }
}

fn read_spilled_image(reference: &SpilledImageRef) -> ContentBlock {
    match std::fs::read_to_string(&reference.path) {
        Ok(data) => ContentBlock::Image(ImageContent {
            data,
            mime_type: reference.mime_type.clone(),
        }),
        Err(error) => ContentBlock::Text(TextContent {
            text: format!(
                "[{} image ({} base64 bytes) of this settled cell is no longer available: {} ({})]",
                reference.mime_type,
                reference.length,
                reference.path.display(),
                error_code(&error)
            ),
            audience: None,
            text_signature: None,
        }),
    }
}

/// Node's `error.code` spelling for the common file errors.
fn error_code(error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::NotFound => "ENOENT".into(),
        std::io::ErrorKind::PermissionDenied => "EACCES".into(),
        _ => error.to_string(),
    }
}
