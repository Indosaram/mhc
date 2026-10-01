//! Port of footer-layout.ts.
use maho_tui::utils::{truncate_to_width, visible_width};
#[derive(Debug, Clone)]
pub struct FooterSegment {
    pub plain: String,
    pub colored: String,
}
#[derive(Debug, Clone)]
pub struct FooterRightLabel {
    pub minimal: FooterSegment,
    pub full: Option<FooterSegment>,
}
pub struct FooterLayoutInput<'a> {
    pub width: usize,
    pub anchor: &'a [FooterSegment],
    pub pwd_index: usize,
    pub middle: &'a [FooterSegment],
    pub tail: &'a FooterSegment,
    pub right: &'a FooterRightLabel,
    pub separator: &'a str,
    pub min_padding: usize,
    pub ellipsis_marker: &'a FooterSegment,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FooterLayout {
    Full {
        use_full_right: bool,
    },
    MiddleElided {
        kept_middle_count: usize,
        show_marker: bool,
        use_full_right: bool,
    },
    PwdElided {
        pwd_plain: String,
        kept_middle_count: usize,
        show_marker: bool,
        use_full_right: bool,
    },
    LeftElided {
        left_plain: String,
    },
    RightTruncated {
        right_plain: String,
    },
}
fn segments_width(segments: &[&FooterSegment], separator: &str) -> usize {
    segments
        .iter()
        .map(|s| visible_width(&s.plain))
        .sum::<usize>()
        + visible_width(separator) * segments.len().saturating_sub(1)
}
pub fn elide_head(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if visible_width(text) <= max_width {
        return text.into();
    }
    if max_width == 1 {
        return "…".into();
    }
    let mut kept = Vec::new();
    let mut used = 0;
    for c in text.chars().rev() {
        let w = visible_width(&c.to_string());
        if used + w > max_width - 1 {
            break;
        }
        kept.push(c);
        used += w;
    }
    format!("…{}", kept.into_iter().rev().collect::<String>())
}
pub fn plan_footer_layout(input: &FooterLayoutInput<'_>) -> Result<FooterLayout, std::io::Error> {
    let pwd = input.anchor.get(input.pwd_index).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("pwdIndex {} is outside the footer anchor", input.pwd_index),
        )
    })?;
    let prefer_pwd = visible_width(&pwd.plain) > input.width / 3;
    let plan = |right: &FooterSegment, use_full_right: bool| {
        let mut candidates = vec![(input.middle.len(), false)];
        candidates.extend((0..input.middle.len()).rev().map(|i| (i, true)));
        candidates.push((0, false));
        for (kept_middle_count, show_marker) in candidates {
            let mut left: Vec<_> = input
                .anchor
                .iter()
                .chain(input.middle.iter().take(kept_middle_count))
                .collect();
            if show_marker {
                left.push(input.ellipsis_marker);
            }
            left.push(input.tail);
            if segments_width(&left, input.separator)
                + input.min_padding
                + visible_width(&right.plain)
                <= input.width
            {
                return Some(if kept_middle_count == input.middle.len() {
                    FooterLayout::Full { use_full_right }
                } else {
                    FooterLayout::MiddleElided {
                        kept_middle_count,
                        show_marker,
                        use_full_right,
                    }
                });
            }
            if prefer_pwd || (!use_full_right && kept_middle_count == 0 && !show_marker) {
                let rest: Vec<_> = input
                    .anchor
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != input.pwd_index)
                    .map(|(_, s)| s)
                    .chain(input.middle.iter().take(kept_middle_count))
                    .chain(if show_marker {
                        Some(input.ellipsis_marker)
                    } else {
                        None
                    })
                    .chain(std::iter::once(input.tail))
                    .collect();
                let budget = input.width.saturating_sub(
                    input.min_padding
                        + visible_width(&right.plain)
                        + visible_width(input.separator)
                        + segments_width(&rest, input.separator),
                );
                if budget >= 2 {
                    return Some(FooterLayout::PwdElided {
                        pwd_plain: elide_head(&pwd.plain, budget),
                        kept_middle_count,
                        show_marker,
                        use_full_right,
                    });
                }
            }
        }
        None
    };
    if let Some(full) = &input.right.full
        && let Some(p) = plan(full, true)
    {
        return Ok(p);
    }
    if let Some(p) = plan(&input.right.minimal, false) {
        return Ok(p);
    }
    let budget = input
        .width
        .saturating_sub(input.min_padding + visible_width(&input.right.minimal.plain));
    if budget >= 1 {
        let text = input
            .anchor
            .iter()
            .chain(std::iter::once(input.tail))
            .map(|s| s.plain.as_str())
            .collect::<Vec<_>>()
            .join(input.separator);
        Ok(FooterLayout::LeftElided {
            left_plain: elide_head(&text, budget),
        })
    } else {
        Ok(FooterLayout::RightTruncated {
            right_plain: truncate_to_width(&input.right.minimal.plain, input.width, "", false),
        })
    }
}
