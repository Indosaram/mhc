//! Pane and window width lookup.

use crate::tmux_utils::deps::{GetPaneDimensionsDeps, strings};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneDimensions {
    pub pane_width: u32,
    pub window_width: u32,
}

/// Parse tmux's `"<a>,<b>"` numeric pair (an empty field counts as 0, like `Number("")`).
pub(crate) fn parse_number_pair(output: &str) -> Option<(u32, u32)> {
    let mut fields = output.trim().split(',').map(|field| {
        let field = field.trim();
        if field.is_empty() {
            Some(0)
        } else {
            field.parse().ok()
        }
    });
    Some((fields.next()??, fields.next()??))
}

pub fn get_pane_dimensions(pane_id: &str, deps: &GetPaneDimensionsDeps) -> Option<PaneDimensions> {
    let tmux = (deps.get_tmux_path)()?;
    let result = deps.run(
        &tmux,
        &strings(&[
            "display",
            "-p",
            "-t",
            pane_id,
            "#{pane_width},#{window_width}",
        ]),
    );
    if result.exit_code != 0 {
        return None;
    }
    let (pane_width, window_width) = parse_number_pair(&result.output)?;
    Some(PaneDimensions {
        pane_width,
        window_width,
    })
}
