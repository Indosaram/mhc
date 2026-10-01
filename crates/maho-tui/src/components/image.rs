//! Port of senpi `packages/tui/src/components/image.ts`.

use std::rc::Rc;

use crate::terminal_image::{
    allocate_image_id, get_capabilities, get_cell_dimensions, get_image_dimensions, image_fallback,
    render_image, ImageRenderOptions,
};
use crate::terminal_text::ImageDimensions;
use crate::tui::Component;
use crate::utils::truncate_to_width;

pub type FallbackColorFn = Rc<dyn Fn(&str) -> String>;

pub struct ImageTheme {
    pub fallback_color: FallbackColorFn,
}

#[derive(Clone, Default)]
pub struct ImageOptions {
    pub max_width_cells: Option<u32>,
    pub max_height_cells: Option<u32>,
    pub filename: Option<String>,
    /// Kitty image ID. If provided, reuses this ID (for animations/updates).
    pub image_id: Option<u32>,
}

pub struct Image {
    base64_data: String,
    mime_type: String,
    dimensions: ImageDimensions,
    theme: ImageTheme,
    options: ImageOptions,
    image_id: Option<u32>,
    cached_lines: Option<Vec<String>>,
    cached_width: Option<usize>,
}

impl Image {
    pub fn new(
        base64_data: impl Into<String>,
        mime_type: impl Into<String>,
        theme: ImageTheme,
        options: ImageOptions,
        dimensions: Option<ImageDimensions>,
    ) -> Self {
        let base64_data = base64_data.into();
        let mime_type = mime_type.into();
        let dimensions = dimensions
            .or_else(|| get_image_dimensions(&base64_data, &mime_type))
            .unwrap_or(ImageDimensions {
                width_px: 800,
                height_px: 600,
            });
        let image_id = options.image_id;
        Self {
            base64_data,
            mime_type,
            dimensions,
            theme,
            options,
            image_id,
            cached_lines: None,
            cached_width: None,
        }
    }

    /// Get the Kitty image ID used by this image (if any).
    pub fn get_image_id(&self) -> Option<u32> {
        self.image_id
    }

    pub fn invalidate(&mut self) {
        self.cached_lines = None;
        self.cached_width = None;
    }

    fn fallback_line(&self, width: usize) -> String {
        let fallback = image_fallback(
            &self.mime_type,
            Some(self.dimensions),
            self.options.filename.as_deref(),
        );
        truncate_to_width(&(self.theme.fallback_color)(&fallback), width, "", false)
    }
}

impl Component for Image {
    fn render(&mut self, width: usize) -> Vec<String> {
        if let (Some(lines), Some(cached_width)) = (&self.cached_lines, self.cached_width)
            && cached_width == width
        {
            return lines.clone();
        }

        let max_width = width
            .saturating_sub(2)
            .min(self.options.max_width_cells.unwrap_or(60) as usize)
            .max(1);
        let cell_dimensions = get_cell_dimensions();
        let default_max_height = ((max_width as f64 * f64::from(cell_dimensions.width_px))
            / f64::from(cell_dimensions.height_px))
        .ceil()
        .max(1.0) as u32;
        let max_height = self.options.max_height_cells.unwrap_or(default_max_height);

        let caps = get_capabilities();
        let lines: Vec<String> = match caps.images {
            Some(protocol) => {
                let kitty = protocol == crate::terminal_capabilities::ImageProtocol::Kitty;
                if kitty && self.image_id.is_none() {
                    self.image_id = Some(allocate_image_id());
                }
                let result = render_image(
                    &self.base64_data,
                    self.dimensions,
                    &ImageRenderOptions {
                        max_width_cells: Some(max_width as u32),
                        max_height_cells: Some(max_height),
                        preserve_aspect_ratio: true,
                        image_id: self.image_id,
                        move_cursor: false,
                    },
                );

                match result {
                    Some(result) => {
                        // Store the image ID for later cleanup.
                        if let Some(image_id) = result.image_id {
                            self.image_id = Some(image_id);
                        }

                        if kitty && result.lines.is_some() {
                            // Unicode placeholder placement (tmux passthrough): each row is a
                            // real text line of placeholder cells; the first row also carries
                            // the virtual-placement transmission.
                            result.lines.unwrap_or_default()
                        } else if kitty {
                            // For Kitty: C=1 prevents cursor movement.
                            // Don't need the cursor movement.
                            let mut lines = vec![result.sequence];

                            // Return `rows` lines so TUI accounts for image height.
                            for _ in 0..result.rows.saturating_sub(1) {
                                lines.push(String::new());
                            }
                            lines
                        } else {
                            // Return `rows` lines so TUI accounts for image height.
                            // First (rows-1) lines are empty and cleared before the image is drawn.
                            // Last line: move cursor back up, draw the image, then move back down
                            // so TUI cursor accounting stays inside the scroll area.
                            let mut lines: Vec<String> = Vec::new();
                            for _ in 0..result.rows.saturating_sub(1) {
                                lines.push(String::new());
                            }
                            let row_offset = result.rows.saturating_sub(1);
                            let move_up = if row_offset > 0 {
                                format!("\x1b[{row_offset}A")
                            } else {
                                String::new()
                            };
                            lines.push(format!("{move_up}{}", result.sequence));
                            lines
                        }
                    }
                    None => vec![self.fallback_line(width)],
                }
            }
            None => vec![self.fallback_line(width)],
        };

        self.cached_lines = Some(lines.clone());
        self.cached_width = Some(width);

        lines
    }
}
