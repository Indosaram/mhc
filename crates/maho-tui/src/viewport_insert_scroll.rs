//! Port of senpi `packages/tui/src/viewport-insert-scroll.ts`.

use crate::terminal_image::is_image_line;
use crate::viewport_shift::shifted_block_moved;

// Keep scroll-region wins cheap when a few visible rows mutate during append streaming.
const MAX_SCROLL_DIFF_ROWS: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutatedRow {
    pub screen_row: usize,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewportInsertScrollPlan {
    pub viewport_top: usize,
    pub region_bottom: usize,
    pub inserted_rows: Vec<String>,
    pub mutated_rows: Vec<MutatedRow>,
}

fn viewport_rows(lines: &[String], viewport_top: usize, height: usize) -> Vec<String> {
    (0..height)
        .map(|row| lines.get(viewport_top + row).cloned().unwrap_or_default())
        .collect()
}

/// Plan a scroll-region update for a frame that grew by `line_count_delta` rows below a stable bottom suffix.
///
/// Scrolling the region pushes the old top `line_count_delta` visible rows into the terminal's scrollback as
/// they are, so the plan only holds when the new frame keeps exactly those rows there. Rows inserted at or
/// above the viewport top break that (senpi#3649), so such a frame takes the general repaint paths instead.
/// Identical rows can make the departing rows equal the new ones by coincidence, so a frame that reads as
/// rows moved down from above (`shifted_block_moved`) is refused too.
pub fn plan_viewport_insert_scroll(
    previous_lines: &[String],
    new_lines: &[String],
    prev_viewport_top: usize,
    height: usize,
    line_count_delta: isize,
) -> Option<ViewportInsertScrollPlan> {
    if line_count_delta <= 0 || line_count_delta as usize >= height {
        return None;
    }
    let line_count_delta = line_count_delta as usize;

    let max_viewport_top = new_lines.len().saturating_sub(height);
    let viewport_top = max_viewport_top.min(prev_viewport_top + line_count_delta);
    if viewport_top <= prev_viewport_top {
        return None;
    }

    let previous_visible = viewport_rows(previous_lines, prev_viewport_top, height);
    let next_visible = viewport_rows(new_lines, viewport_top, height);
    if previous_visible.iter().any(|line| is_image_line(line))
        || next_visible.iter().any(|line| is_image_line(line))
    {
        return None;
    }

    let scrolled_out = viewport_top - prev_viewport_top;
    for row in 0..scrolled_out {
        if Some(&previous_visible[row]) != new_lines.get(prev_viewport_top + row) {
            return None;
        }
    }
    if shifted_block_moved(
        previous_lines,
        new_lines,
        prev_viewport_top,
        scrolled_out,
        height,
    ) {
        return None;
    }

    let mut stable_suffix_rows = 0;
    while stable_suffix_rows < height
        && previous_visible[height - stable_suffix_rows - 1]
            == next_visible[height - stable_suffix_rows - 1]
    {
        stable_suffix_rows += 1;
    }

    let region_height = height - stable_suffix_rows;
    if region_height == 0 || region_height < line_count_delta {
        return None;
    }

    let mut mutated_rows = Vec::new();
    for row in 0..region_height - line_count_delta {
        if previous_visible[row + line_count_delta] != next_visible[row] {
            mutated_rows.push(MutatedRow {
                screen_row: row,
                content: next_visible[row].clone(),
            });
            if mutated_rows.len() > MAX_SCROLL_DIFF_ROWS {
                return None;
            }
        }
    }

    Some(ViewportInsertScrollPlan {
        viewport_top,
        region_bottom: region_height - 1,
        inserted_rows: next_visible[region_height - line_count_delta..region_height].to_vec(),
        mutated_rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_string()).collect()
    }

    #[test]
    fn plans_a_scroll_for_an_append_above_a_stable_footer() {
        let previous = lines(&["a", "b", "c", "footer"]);
        let next = lines(&["a", "b", "c", "d", "footer"]);
        let plan = plan_viewport_insert_scroll(&previous, &next, 1, 3, 1).unwrap();
        assert_eq!(plan.viewport_top, 2);
        assert_eq!(plan.region_bottom, 1);
        assert_eq!(plan.inserted_rows, lines(&["d"]));
        assert!(plan.mutated_rows.is_empty());
    }

    #[test]
    fn refuses_rows_inserted_above_the_viewport() {
        let previous = lines(&["a", "b", "c", "d", "footer"]);
        let next = lines(&["a", "X", "b", "c", "d", "footer"]);
        assert_eq!(plan_viewport_insert_scroll(&previous, &next, 2, 3, 1), None);
    }

    #[test]
    fn refuses_non_growth_and_full_screen_growth() {
        let previous = lines(&["a", "b"]);
        assert_eq!(
            plan_viewport_insert_scroll(&previous, &previous, 0, 2, 0),
            None
        );
        assert_eq!(
            plan_viewport_insert_scroll(&previous, &previous, 0, 2, 2),
            None
        );
    }
}
