//! Port of `packages/coding-agent/src/utils/image-mime.ts`.

pub const IMAGE_TYPE_SNIFF_BYTES: usize = 4100;

pub use maho_tools::read::detect_supported_image_mime_type;
