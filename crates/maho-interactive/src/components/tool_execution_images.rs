//! Port of `components/tool-execution-images.ts`.
//!
//! senpi converts non-PNG images to PNG for the Kitty protocol through `utils/image-convert.ts`
//! (photon); the port uses `maho_core::utils::image_convert::convert_to_png`. senpi's conversion
//! is a promise; the port converts synchronously, which needs no generation guard.
//!
//! Payloads are shared, never copied: the tool result is held behind an `Rc`, each base64 payload
//! is materialized as one `Rc<str>` per `(index, payload)` in the block cache below, and the
//! rendered [`Image`] components borrow that same allocation through `Image::new_shared`. The
//! conversion cache keeps only the converted payload plus a source fingerprint, so a converted
//! image is never retained twice and never applied to changed source bytes.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use maho_tools::definition::ToolContent;
use maho_tui::components::image::{Image, ImageOptions, ImageTheme};
use maho_tui::components::spacer::Spacer;
use maho_tui::components::text::Text;
use maho_tui::terminal_capabilities::ImageProtocol;
use maho_tui::terminal_image::get_capabilities;
use maho_tui::terminal_text::sanitize_terminal_label;
use maho_tui::tui::{Component, Container};

use super::tool_execution_types::ToolExecutionResult;
use crate::theme::{Theme, ThemeColor};

#[derive(Clone, Debug)]
pub struct ToolExecutionImageOptions {
    pub show_images: bool,
    pub max_width_cells: u32,
    pub show_renderer_fallback: bool,
}

impl Default for ToolExecutionImageOptions {
    fn default() -> Self {
        Self {
            show_images: true,
            max_width_cells: 60,
            show_renderer_fallback: false,
        }
    }
}

/// Identity of the source bytes a conversion was produced from.
///
/// `len` is the payload length and `hash` is a 64-bit FNV-1a over the payload bytes followed by
/// the mime type bytes. A 64-bit FNV collision between two distinct payloads would reuse a stale
/// conversion (and so show the wrong image); that is the accepted trade-off for not retaining a
/// second copy of every source payload, which is what this type exists to avoid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SourceFingerprint {
    len: usize,
    hash: u64,
}

impl SourceFingerprint {
    fn of(data: &str, mime_type: &str) -> Self {
        const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
        const PRIME: u64 = 0x0000_0100_0000_01b3;
        let mut hash = OFFSET_BASIS;
        for byte in data.bytes().chain(mime_type.bytes()) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(PRIME);
        }
        Self {
            len: data.len(),
            hash,
        }
    }
}

/// One shared image payload: the `Rc<str>` handed to `Image::new_shared` plus the identity of the
/// source bytes it was built from.
#[derive(Clone)]
struct CachedBlock {
    /// `(address, length)` of the source payload. Entries are only ever built from the current
    /// [`ToolExecutionResult`], which is immutable behind its `Rc` and replaced wholesale by
    /// `update_result` (which clears this cache), so a matching key means the same bytes without
    /// re-scanning the payload.
    data_key: (usize, usize),
    mime_key: (usize, usize),
    data: Rc<str>,
    mime: Rc<str>,
    source: SourceFingerprint,
}

/// A converted payload: the PNG bytes and mime only. The source copy is deliberately absent - it
/// is recognized by `source` instead of being stored.
struct ConvertedImage {
    source: SourceFingerprint,
    data: Rc<str>,
    mime: Rc<str>,
}

pub struct ToolExecutionImages {
    content: Container,
    result: Option<Rc<ToolExecutionResult>>,
    converted: HashMap<usize, ConvertedImage>,
    /// Shared payloads by image index, so repeated `image_blocks` calls reuse allocations.
    blocks: RefCell<HashMap<usize, CachedBlock>>,
    options: ToolExecutionImageOptions,
    theme: Theme,
}

impl ToolExecutionImages {
    pub fn new(theme: Theme) -> Self {
        Self {
            content: Container::new(),
            result: None,
            converted: HashMap::new(),
            blocks: RefCell::new(HashMap::new()),
            options: ToolExecutionImageOptions::default(),
            theme,
        }
    }

    pub fn update_options(&mut self, options: ToolExecutionImageOptions) {
        let normalized = ToolExecutionImageOptions {
            max_width_cells: options.max_width_cells.max(1),
            ..options
        };
        if self.options.show_images == normalized.show_images
            && self.options.max_width_cells == normalized.max_width_cells
            && self.options.show_renderer_fallback == normalized.show_renderer_fallback
        {
            return;
        }
        self.options = normalized;
        self.rebuild();
    }

    pub fn update_result(&mut self, result: Rc<ToolExecutionResult>) {
        // Replacing the result invalidates the cached payloads; the conversion cache is pruned
        // against the new content so conversions that still describe their image survive (the
        // partial-result path re-sends the same images) and stale ones are dropped.
        self.result = Some(result);
        self.blocks.get_mut().clear();
        self.prune_converted_images();
        self.rebuild();
        self.maybe_convert_images_for_kitty();
    }

    /// Image payloads in content order as `(base64, mime)`. Every call returns clones of the same
    /// `Rc<str>` allocations, so no payload bytes are copied.
    fn image_blocks(&self) -> Vec<(Rc<str>, Rc<str>)> {
        self.cached_blocks()
            .into_iter()
            .map(|block| (block.data, block.mime))
            .collect()
    }

    /// Source fingerprints for the current image payloads, in image order. Refreshes the block
    /// cache first, so each payload is hashed once per `(index, payload)`.
    fn block_fingerprints(&self) -> Vec<SourceFingerprint> {
        self.cached_blocks()
            .into_iter()
            .map(|block| block.source)
            .collect()
    }

    /// The current image payloads, in content order, as shared `Rc<str>`s.
    ///
    /// The returned `Vec` is built fresh each call (it is a few words per image), but the payload
    /// allocations inside it are reused: entry `index` is only rebuilt when the payload at that
    /// index is no longer the one it was built from.
    fn cached_blocks(&self) -> Vec<CachedBlock> {
        let Some(result) = self.result.as_ref() else {
            self.blocks.borrow_mut().clear();
            return Vec::new();
        };
        let mut cache = self.blocks.borrow_mut();
        let mut blocks: Vec<CachedBlock> = Vec::new();
        for part in &result.content {
            let ToolContent::Image { data, mime_type } = part else {
                continue;
            };
            let index = blocks.len();
            let data_key = (data.as_ptr() as usize, data.len());
            let mime_key = (mime_type.as_ptr() as usize, mime_type.len());
            let cached = cache
                .get(&index)
                .filter(|cached| cached.data_key == data_key && cached.mime_key == mime_key)
                .cloned();
            let block = match cached {
                Some(block) => block,
                None => {
                    let block = CachedBlock {
                        data_key,
                        mime_key,
                        data: Rc::from(data.as_str()),
                        mime: Rc::from(mime_type.as_str()),
                        source: SourceFingerprint::of(data, mime_type),
                    };
                    cache.insert(index, block.clone());
                    block
                }
            };
            blocks.push(block);
        }
        // Images past the end of the current content are gone; drop their shared payloads.
        let count = blocks.len();
        cache.retain(|index, _| *index < count);
        blocks
    }

    fn prune_converted_images(&mut self) {
        let sources = self.block_fingerprints();
        self.converted
            .retain(|index, entry| sources.get(*index) == Some(&entry.source));
    }

    fn maybe_convert_images_for_kitty(&mut self) {
        if get_capabilities().images != Some(ImageProtocol::Kitty) || self.result.is_none() {
            return;
        }
        let blocks = self.image_blocks();
        let fingerprints = self.block_fingerprints();
        let mut changed = false;
        for (index, ((source_data, source_mime_type), fingerprint)) in
            blocks.iter().zip(fingerprints.iter().copied()).enumerate()
        {
            if source_data.is_empty()
                || source_mime_type.is_empty()
                || &**source_mime_type == "image/png"
            {
                continue;
            }
            if self
                .converted
                .get(&index)
                .is_some_and(|entry| entry.source == fingerprint)
            {
                continue;
            }
            let Some(converted) =
                maho_core::utils::image_convert::convert_to_png(source_data, source_mime_type)
            else {
                continue;
            };
            self.converted.insert(
                index,
                ConvertedImage {
                    source: fingerprint,
                    data: Rc::from(converted.data),
                    mime: Rc::from(converted.mime_type),
                },
            );
            changed = true;
        }
        if changed {
            self.rebuild();
        }
    }

    fn rebuild(&mut self) {
        self.content.clear();
        let capabilities = get_capabilities();
        if !self.options.show_images || capabilities.images.is_none() {
            return;
        }
        let blocks = self.image_blocks();
        let fingerprints = self.block_fingerprints();
        for (index, ((data, mime_type), fingerprint)) in
            blocks.iter().zip(fingerprints.iter().copied()).enumerate()
        {
            if data.is_empty() || mime_type.is_empty() {
                continue;
            }
            let (image_data, image_mime_type) = match self.converted.get(&index) {
                Some(entry) if entry.source == fingerprint => {
                    (Rc::clone(&entry.data), Rc::clone(&entry.mime))
                }
                _ => (Rc::clone(data), Rc::clone(mime_type)),
            };
            if capabilities.images == Some(ImageProtocol::Kitty) && &*image_mime_type != "image/png"
            {
                if self.options.show_renderer_fallback {
                    let text = self.theme.fg(
                        ThemeColor::ToolOutput,
                        &format!("[image: {}]", sanitize_terminal_label(&image_mime_type)),
                    );
                    self.add_image_component(Rc::new(RefCell::new(Text::with_padding(text, 0, 0))));
                }
                continue;
            }
            let theme = self.theme.clone();
            let image = Image::new_shared(
                image_data,
                &*image_mime_type,
                ImageTheme {
                    fallback_color: Rc::new(move |text| theme.fg(ThemeColor::ToolOutput, text)),
                },
                ImageOptions {
                    max_width_cells: Some(self.options.max_width_cells),
                    ..Default::default()
                },
                None,
            );
            self.add_image_component(Rc::new(RefCell::new(image)));
        }
    }

    fn add_image_component(&mut self, component: Rc<RefCell<dyn Component>>) {
        self.content
            .add_child(Rc::new(RefCell::new(Spacer::new(1))));
        self.content.add_child(component);
    }
}

impl Component for ToolExecutionImages {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.content.render(width)
    }
    fn invalidate(&mut self) {
        // The shared payload cache is derived state: drop it so the next call rebuilds the
        // `Rc<str>`s from the current result instead of trusting anything older.
        self.blocks.get_mut().clear();
        self.content.invalidate();
    }
    fn dispose(&mut self) {
        self.content.dispose();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ColorMode;

    fn theme() -> Theme {
        Theme::builtin("dark", ColorMode::Color256).expect("builtin theme")
    }

    fn result(parts: Vec<ToolContent>) -> Rc<ToolExecutionResult> {
        Rc::new(ToolExecutionResult {
            content: parts,
            details: None,
            is_error: false,
        })
    }

    fn image(data: &str, mime_type: &str) -> ToolContent {
        ToolContent::Image {
            data: data.to_owned(),
            mime_type: mime_type.to_owned(),
        }
    }

    fn converted(index_source: SourceFingerprint, data: &str, mime: &str) -> ConvertedImage {
        ConvertedImage {
            source: index_source,
            data: Rc::from(data),
            mime: Rc::from(mime),
        }
    }

    #[test]
    fn image_blocks_returns_source_bytes_from_one_shared_allocation() {
        let mut component = ToolExecutionImages::new(theme());
        component.update_result(result(vec![
            ToolContent::text("hello"),
            image("aGVsbG8=", "image/png"),
            image("d29ybGQ=", "image/gif"),
        ]));

        let first = component.image_blocks();
        let second = component.image_blocks();

        assert_eq!(first.len(), 2);
        assert_eq!(&*first[0].0, "aGVsbG8=");
        assert_eq!(&*first[0].1, "image/png");
        assert_eq!(&*first[1].0, "d29ybGQ=");
        assert_eq!(&*first[1].1, "image/gif");
        for (left, right) in first.iter().zip(&second) {
            assert!(Rc::ptr_eq(&left.0, &right.0), "payload Rc is reused");
            assert!(Rc::ptr_eq(&left.1, &right.1), "mime Rc is reused");
        }
    }

    #[test]
    fn converted_entry_keeps_only_converted_payload_and_a_fingerprint() {
        let mut component = ToolExecutionImages::new(theme());
        component.update_result(result(vec![image("source-one", "image/gif")]));
        let first = component.block_fingerprints();
        assert_eq!(first.len(), 1);

        component
            .converted
            .insert(0, converted(first[0], "cG5nLW9uZQ==", "image/png"));

        let entry = component.converted.get(&0).expect("cached conversion");
        // Only converted data/mime plus the source fingerprint are reachable; the source payload
        // is not stored (there is no source-data field to read).
        assert_eq!(&*entry.data, "cG5nLW9uZQ==");
        assert_eq!(&*entry.mime, "image/png");
        assert_eq!(entry.source, first[0]);
        assert!(
            std::mem::size_of::<ConvertedImage>() < 64,
            "entry holds no source payload copy"
        );

        // A changed source payload must invalidate the stale conversion.
        component.update_result(result(vec![image("source-two", "image/gif")]));
        assert!(
            !component
                .converted
                .values()
                .any(|entry| entry.source == first[0]),
            "stale conversion is dropped, never reused"
        );
        let second = component.block_fingerprints();
        assert_ne!(first[0], second[0]);

        // The mime type is part of the fingerprint too.
        assert_ne!(
            SourceFingerprint::of("source-two", "image/gif"),
            SourceFingerprint::of("source-two", "image/jpeg")
        );
    }

    #[test]
    fn update_result_replaces_images_and_clears_stale_conversions() {
        let mut component = ToolExecutionImages::new(theme());
        component.update_result(result(vec![
            image("b25l", "image/gif"),
            image("dHdv", "image/gif"),
        ]));
        let fingerprints = component.block_fingerprints();
        assert_eq!(fingerprints.len(), 2);
        for (index, fingerprint) in fingerprints.iter().enumerate() {
            component
                .converted
                .insert(index, converted(*fingerprint, "cG5n", "image/png"));
        }
        assert_eq!(component.converted.len(), 2);

        component.update_result(result(vec![image("dGhyZWU=", "image/png")]));

        let blocks = component.image_blocks();
        assert_eq!(blocks.len(), 1);
        assert_eq!(&*blocks[0].0, "dGhyZWU=");
        assert!(component.converted.is_empty(), "stale conversions dropped");
    }
}
