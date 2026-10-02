use maho_interactive::{components::{shortcut_overlay::ShortcutOverlay, show_images_selector::ShowImagesSelectorComponent}, theme::{Theme, ColorMode}};
use maho_tui::tui::Component;

fn main() {
    let width = std::env::args().nth(1).map(|value| value.parse::<usize>().expect("width")).unwrap_or(80);
    let theme = Theme::builtin("dark", ColorMode::Truecolor).expect("theme");
    let mut shortcut = ShortcutOverlay::new(&theme);
    let mut images = ShowImagesSelectorComponent::new(&theme, false, Box::new(|selected| println!("Selected image setting: {selected}")), Box::new(|| {}));
    for line in shortcut.render(width) { println!("{line}\r"); }
    for line in images.render(width) { println!("{line}\r"); }
    images.handle_input("\r");
}
