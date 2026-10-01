use maho_tui::components::image::{Image, ImageOptions, ImageTheme};
use maho_tui::terminal_image::get_capabilities;
use maho_tui::terminal_text::ImageDimensions;
use maho_tui::tui::Component;
use std::rc::Rc;

fn main() {
    assert!(get_capabilities().images.is_none());
    let mut image = Image::new(
        "",
        "image/png",
        ImageTheme { fallback_color: Rc::new(str::to_string) },
        ImageOptions { filename: Some("diagram.png".to_string()), ..ImageOptions::default() },
        Some(ImageDimensions { width_px: 640, height_px: 480 }),
    );
    for line in image.render(80) {
        println!("{line}");
    }
}
