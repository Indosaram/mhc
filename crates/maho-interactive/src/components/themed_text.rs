//! Port of senpi `packages/coding-agent/src/modes/interactive/components/themed-text.ts`.
//!
//! Text whose content applies theme colors. Plain `Text` keeps the colors its string was built
//! with, so a theme change would leave it stale. This rebuilds the string from `build` after every
//! invalidation, which the UI performs on theme changes. `build` must return the same content
//! each time, apart from colors.

use maho_tui::components::text::Text;
use maho_tui::tui::Component;

pub struct ThemedText {
    text: Text,
    build: Box<dyn FnMut() -> String>,
    stale: bool,
}

impl ThemedText {
    pub fn new(build: impl FnMut() -> String + 'static) -> Self {
        Self::with_padding(build, 1, 1)
    }

    pub fn with_padding(
        build: impl FnMut() -> String + 'static,
        padding_x: usize,
        padding_y: usize,
    ) -> Self {
        Self {
            text: Text::with_padding("", padding_x, padding_y),
            build: Box::new(build),
            stale: true,
        }
    }
}

impl Component for ThemedText {
    fn invalidate(&mut self) {
        self.text.invalidate();
        self.stale = true;
    }

    fn render(&mut self, width: usize) -> Vec<String> {
        if self.stale {
            self.stale = false;
            self.text.set_text((self.build)());
        }
        self.text.render(width)
    }
}

#[cfg(test)]
mod tests {
    //! Port of senpi `test/themed-text.test.ts`; the global theme switch is a shared cell the
    //! `build` closure reads, as senpi's `build` reads the global `theme`.

    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use super::*;
    use crate::theme::theme::{Theme, ThemeColor};

    #[test]
    fn builds_lazily_and_rebuilds_with_the_current_theme_after_invalidation() {
        let mode = crate::theme::ColorMode::Truecolor;
        let current = Rc::new(RefCell::new(
            Theme::builtin("dark", mode).expect("dark theme"),
        ));
        let builds = Rc::new(Cell::new(0));
        let mut text = ThemedText::new({
            let current = Rc::clone(&current);
            let builds = Rc::clone(&builds);
            move || {
                builds.set(builds.get() + 1);
                current.borrow().fg(ThemeColor::Accent, "hello")
            }
        });
        assert_eq!(builds.get(), 0);
        let dark = text.render(20).join("");

        *current.borrow_mut() = Theme::builtin("light", mode).expect("light theme");
        assert_eq!(text.render(20).join(""), dark);
        text.invalidate();
        let light_accent = current.borrow().fg(ThemeColor::Accent, "");
        let ansi = light_accent.trim_end_matches("\x1b[39m");
        assert!(text.render(20).join("").contains(ansi));
        assert_ne!(text.render(20).join(""), dark);
        assert_eq!(builds.get(), 2);
    }
}
