//! Port of senpi `packages/tui/src/viewport-shift.ts`.

fn at(lines: &[String], row: isize) -> Option<&str> {
    usize::try_from(row)
        .ok()
        .and_then(|row| lines.get(row))
        .map(String::as_str)
}

/// Whether the new frame reads as the old one with `shift` rows inserted above `prev_viewport_top`: the rows at
/// `prev_viewport_top..+shift` are the old rows right above it, and walking up through the block of rows that keeps
/// matching that reading finds at least one row that really moved. Identical rows (blank or repeated lines) match
/// both readings and prove nothing; the walk stops at the first row the shift does not explain.
pub fn shifted_block_moved(
    previous_lines: &[String],
    new_lines: &[String],
    prev_viewport_top: usize,
    shift: usize,
    height: usize,
) -> bool {
    if shift == 0 || shift > prev_viewport_top {
        return false;
    }
    let (top, shift_i, height_i) = (prev_viewport_top as isize, shift as isize, height as isize);
    for row in top..top + shift_i {
        let Some(scrolled) = at(previous_lines, row - shift_i) else {
            return false;
        };
        if Some(scrolled) != at(new_lines, row) {
            return false;
        }
    }
    let scan_floor = shift_i.max(top - 2 * height_i);
    let mut row = top + shift_i - 1;
    while row >= scan_floor {
        let line = at(new_lines, row);
        if line != at(previous_lines, row - shift_i) {
            return false;
        }
        if line != at(previous_lines, row) {
            return true;
        }
        row -= 1;
    }
    false
}

/// How far the last frame's visible rows moved down in the new frame because rows were inserted above them.
///
/// A held repaint writes the frame from a start row and lets the rows above the screen scroll into the
/// terminal's scrollback. Rows that sat above the old viewport are already there, so after an insertion
/// above the viewport (a live tool card growing over a tall /btw panel, senpi#3649) the repaint must start
/// where the old viewport's rows went, or every frame pushes another copy of them.
///
/// A shift counts only when the rows it would skip are exactly the rows already in the scrollback right above
/// the old viewport, the contiguous block of rows above the viewport that matches the shift holds at least one
/// row that really moved, and most of the old visible rows reappear at it. A majority alone is not enough: a
/// tall stable live area matches at any offset after an append, while rows re-laid out in place (a streamed
/// table, #2836) are new rows that must still scroll into the scrollback, so the start stays at the old
/// viewport top.
pub fn inserted_above_viewport(
    previous_lines: &[String],
    new_lines: &[String],
    prev_viewport_top: usize,
    height: usize,
    line_count_delta: isize,
) -> usize {
    if line_count_delta <= 0 {
        return 0;
    }
    let visible_matches_at = |shift: usize| -> usize {
        (0..height)
            .filter(|row| {
                let previous = previous_lines.get(prev_viewport_top + row);
                previous.is_some() && previous == new_lines.get(prev_viewport_top + shift + row)
            })
            .count()
    };
    let mut best = 0;
    let mut best_matches = visible_matches_at(0);
    let max_shift = (line_count_delta as usize).min(prev_viewport_top);
    for shift in 1..=max_shift {
        if !shifted_block_moved(previous_lines, new_lines, prev_viewport_top, shift, height) {
            continue;
        }
        let matches = visible_matches_at(shift);
        if matches > best_matches {
            best = shift;
            best_matches = matches;
        }
    }
    if best_matches * 2 > height { best } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_string()).collect()
    }

    #[test]
    fn detects_rows_inserted_above_the_viewport() {
        let previous = lines(&["a", "b", "c", "d", "e"]);
        let next = lines(&["a", "b", "X", "c", "d", "e"]);
        assert!(shifted_block_moved(&previous, &next, 3, 1, 2));
        assert_eq!(inserted_above_viewport(&previous, &next, 3, 2, 1), 1);
    }

    #[test]
    fn identical_rows_prove_nothing() {
        let previous = lines(&["a", "a", "a", "a"]);
        let next = lines(&["a", "a", "a", "a", "a"]);
        assert!(!shifted_block_moved(&previous, &next, 2, 1, 2));
    }

    #[test]
    fn an_append_below_keeps_the_start() {
        let previous = lines(&["a", "b", "c", "d"]);
        let next = lines(&["a", "b", "c", "d", "e"]);
        assert_eq!(inserted_above_viewport(&previous, &next, 2, 2, 1), 0);
    }
}
