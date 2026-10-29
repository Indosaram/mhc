pub const DEFAULT_MAX_CELLS: usize = 16_000_000;

pub fn is_ordered_subsequence(needle: &str, haystack: &str) -> bool {
    let mut chars = needle.chars().peekable();
    for ch in haystack.chars() {
        if chars.peek() == Some(&ch) {
            chars.next();
        }
    }
    chars.peek().is_none()
}

pub fn merge_insertions(base: &str, incoming: &str) -> String {
    merge_insertions_with_budget(base, incoming, DEFAULT_MAX_CELLS)
}

pub fn merge_insertions_with_budget(base: &str, incoming: &str, max_cells: usize) -> String {
    if base == incoming || incoming.is_empty() {
        return base.into();
    }
    if base.is_empty() {
        return incoming.into();
    }
    let a: Vec<&str> = base
        .strip_suffix('\n')
        .unwrap_or(base)
        .split('\n')
        .collect();
    let b: Vec<&str> = incoming
        .strip_suffix('\n')
        .unwrap_or(incoming)
        .split('\n')
        .collect();
    let cells = (a.len() + 1).checked_mul(b.len() + 1);
    let mut lines = Vec::new();
    match cells.filter(|&count| count <= max_cells) {
        None => {
            lines.extend_from_slice(&a);
            lines.extend_from_slice(&b);
        }
        Some(count) => {
            let width = b.len() + 1;
            let mut dp = vec![0_usize; count];
            for i in (0..a.len()).rev() {
                for j in (0..b.len()).rev() {
                    dp[i * width + j] = if a[i] == b[j] {
                        dp[(i + 1) * width + j + 1] + 1
                    } else {
                        dp[(i + 1) * width + j].max(dp[i * width + j + 1])
                    };
                }
            }
            let (mut i, mut j) = (0, 0);
            let mut anchors = Vec::new();
            while i < a.len() && j < b.len() {
                if a[i] == b[j] {
                    anchors.push(a[i]);
                    i += 1;
                    j += 1;
                } else if dp[(i + 1) * width + j] >= dp[i * width + j + 1] {
                    i += 1;
                } else {
                    j += 1;
                }
            }
            (i, j) = (0, 0);
            for anchor in anchors {
                while i < a.len() && a[i] != anchor {
                    lines.push(a[i]);
                    i += 1;
                }
                while j < b.len() && b[j] != anchor {
                    lines.push(b[j]);
                    j += 1;
                }
                lines.push(anchor);
                i += 1;
                j += 1;
            }
            lines.extend_from_slice(&a[i..]);
            lines.extend_from_slice(&b[j..]);
        }
    }
    let mut out = lines.join("\n");
    if base.ends_with('\n') || incoming.ends_with('\n') {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_lines_survive_when_prose_is_similar() {
        // Given
        let cases = [
            ("", "", ""),
            ("abc", "abc", "abc"),
            ("", "hello\n", "hello\n"),
            ("hello\n", "", "hello\n"),
            ("abc", "abXc", "abc\nabXc"),
            ("a x c", "a y c", "a x c\na y c"),
            (
                "alpha beta\nlast\n",
                "alpha gamma beta\nlast\n",
                "alpha beta\nalpha gamma beta\nlast\n",
            ),
            ("one\ntwo\n", "zero\none\ntwo\n", "zero\none\ntwo\n"),
        ];
        for (base, incoming, expected) in cases {
            // When
            let merged = merge_insertions(base, incoming);
            // Then
            assert_eq!(merged, expected);
        }
    }

    #[test]
    fn both_inputs_remain_subsequences_with_and_without_alignment_budget() {
        // Given
        let inputs = [
            "a\nb",
            "b\na\n",
            "a\na\n",
            "",
            "a",
            "a\n",
            "\n",
            "\u{1f600}\nb\n",
        ];
        for base in inputs {
            for incoming in inputs {
                for budget in [1, DEFAULT_MAX_CELLS] {
                    // When
                    let merged = merge_insertions_with_budget(base, incoming, budget);
                    // Then
                    assert!(is_ordered_subsequence(base, &merged));
                    assert!(is_ordered_subsequence(incoming, &merged));
                }
            }
        }
    }
}
