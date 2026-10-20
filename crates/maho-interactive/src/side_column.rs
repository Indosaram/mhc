use crate::{interactive_ui_host::BoxedComponent, tui_renderer::InteractiveTui};
use maho_ext_api::{ExtensionSideColumn, Theme};
use maho_tui::{
    layout_node::{
        LayoutComponent, LayoutNode, StackAlign, StackBasis, StackDirection, StackLayoutEntry,
        StackLayoutNode,
    },
    tui::{Component, Container},
};
use std::{cell::RefCell, rc::Rc};

pub struct MountedSideColumn {
    original: Rc<RefCell<dyn Component>>,
    root: Rc<RefCell<dyn Component>>,
}
impl MountedSideColumn {
    pub fn mount(
        renderer: &mut InteractiveTui,
        column: &ExtensionSideColumn,
        theme: &Theme,
        dimensions: Rc<std::cell::Cell<(u16, u16)>>,
    ) -> Option<Self> {
        let InteractiveTui::Fullscreen(tui) = renderer else {
            return None;
        };
        // The first mounted root is the layout root, before any overlay roots.
        let original = tui.base.get_mouse_layout_roots().first()?.clone();
        let body: Rc<RefCell<dyn Component>> =
            Rc::new(RefCell::new(BoxedComponent((column.factory)(theme))));
        let mut container = Container::new();
        container.add_child(original.clone());
        container.add_child(body.clone());
        let root: Rc<RefCell<dyn Component>> = Rc::new(RefCell::new(ColumnRoot {
            original: original.clone(),
            body,
            column: column.clone(),
            container,
            dimensions,
        }));
        tui.set_layout_root(Some(root.clone()));
        Some(Self { original, root })
    }
    pub fn dispose(self, renderer: &mut InteractiveTui) {
        if let InteractiveTui::Fullscreen(tui) = renderer
            && tui
                .base
                .get_mouse_layout_roots()
                .first()
                .is_some_and(|root| Rc::ptr_eq(root, &self.root))
        {
            tui.set_layout_root(Some(self.original));
        }
    }
}
struct ColumnRoot {
    original: Rc<RefCell<dyn Component>>,
    body: Rc<RefCell<dyn Component>>,
    column: ExtensionSideColumn,
    container: Container,
    dimensions: Rc<std::cell::Cell<(u16, u16)>>,
}
impl Component for ColumnRoot {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.original.borrow_mut().render(width)
    }
    fn invalidate(&mut self) {
        self.original.borrow_mut().invalidate();
        self.body.borrow_mut().invalidate();
    }
    fn as_container(&self) -> Option<&Container> {
        Some(&self.container)
    }
    fn as_container_mut(&mut self) -> Option<&mut Container> {
        Some(&mut self.container)
    }
    fn as_layout_component(&self) -> Option<&dyn LayoutComponent> {
        Some(self)
    }
}
impl LayoutComponent for ColumnRoot {
    fn layout_node(&self) -> LayoutNode {
        let minimum = self.column.min_columns;
        LayoutNode::Stack(StackLayoutNode {
            direction: StackDirection::HStack,
            gap: 0,
            align: StackAlign::Stretch,
            entries: vec![
                StackLayoutEntry {
                    component: self.original.clone(),
                    basis: Some(StackBasis::Fixed(0)),
                    grow: Some(1),
                    shrink: Some(1),
                    min_size: Some(
                        self.column
                            .min_transcript_columns
                            .min(usize::from(self.dimensions.get().0)),
                    ),
                    max_size: None,
                    visible: None,
                },
                StackLayoutEntry {
                    component: self.body.clone(),
                    basis: Some(StackBasis::Fixed((self.column.width)(usize::from(
                        self.dimensions.get().0,
                    )))),
                    grow: Some(0),
                    shrink: Some(0),
                    min_size: None,
                    max_size: None,
                    visible: Some(Rc::new(move |viewport| viewport.width >= minimum)),
                },
            ],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_tui::layout::render_layout_frame;

    #[test]
    fn native_column_preserves_root_identity_and_tracks_resize() {
        let original: Rc<RefCell<dyn Component>> = Rc::new(RefCell::new(
            maho_tui::components::text::Text::with_padding("transcript", 0, 0),
        ));
        let mut tui = maho_tui::tui_alt_screen::TuiAltScreen::new();
        tui.set_layout_root(Some(original.clone()));
        let mut renderer = InteractiveTui::Fullscreen(Box::new(tui));
        let dimensions = Rc::new(std::cell::Cell::new((160, 24)));
        let column = ExtensionSideColumn {
            factory: std::sync::Arc::new(|_| {
                Box::new(maho_tui::components::text::Text::with_padding(
                    "panel", 0, 0,
                ))
            }),
            width: std::sync::Arc::new(|width| width / 4),
            min_columns: 120,
            min_transcript_columns: 60,
        };
        let mounted = MountedSideColumn::mount(
            &mut renderer,
            &column,
            &Theme::default(),
            dimensions.clone(),
        )
        .expect("fullscreen mounts");
        let root = renderer.base().get_mouse_layout_roots()[0].clone();
        let frame = render_layout_frame(&root, 160, 24);
        assert_eq!(frame.root.children.len(), 2);
        assert!(Rc::ptr_eq(&frame.root.children[0].component, &original));
        assert_eq!(frame.root.children[0].rect.width, 120);
        assert_eq!(frame.root.children[1].rect.width, 40);
        dimensions.set((80, 24));
        let frame = render_layout_frame(&root, 80, 24);
        assert_eq!(frame.root.children.len(), 1);
        assert_eq!(frame.root.children[0].rect.width, 80);
        mounted.dispose(&mut renderer);
        assert!(Rc::ptr_eq(
            &renderer.base().get_mouse_layout_roots()[0],
            &original
        ));
    }
}



Additional project instructions matched for ../mhc-wt/omo-5138/crates/maho-interactive/src/side_column.rs:

Instructions from: /home/projects/mhc-wt/omo-5138/crates/maho-interactive/AGENTS.md
# crates/maho-interactive

## OVERVIEW
Interactive terminal session orchestrator, UI presentation components, and streaming display handlers.

## WHERE TO LOOK
| Task | Location | Notes |
|------|----------|-------|
| Terminal components | `src/components/` | Prompt, status indicators, markdown, diff viewers |
| Streaming reveal & pacing | `src/streaming_reveal.rs` | Token animation and smooth rendering loop integration |
| Streaming pacing config | `src/streaming_reveal_pacing.rs` | Speed governors and rate calculation |
| Content buffering | `src/streaming_reveal_content.rs` | Chunk ingestion and layout preservation |
| Provider error rendering | `src/provider_error_presentation.rs` | Formatted user-facing error notices |
| Session format & tips | `src/session_info_format.rs`, `src/tips/` | Session banners and contextual workflow tips |
| Theming & styles | `src/theme/` | ANSI styling, color palettes, and syntax themes |

## CONVENTIONS
- Component rendering builds upon senpi string/ANSI differential renderer.
- Preserve deterministic layout bounds to prevent terminal flicker during redraws.
- Clean up alternate screen buffers on interrupt or exit signals.
- Workspace lints enforced: zero unwrap across all UI routines.

## ANTI-PATTERNS (THIS COMPONENT)
- Do not introduce ratatui dependencies or ratatui buffer conventions.
- Never write raw unbuffered ANSI escapes outside the managed renderer pipeline.
- Avoid variable timing delays in rendering unit tests.

## NOTES
- Scored 15 due to high file count, complex ANSI rendering state, and direct UX centrality.
- All visual tests use tools/golden fixtures without ratatui.
