//! Port of footer-layout.ts.
use maho_tui::utils::{truncate_to_width, visible_width};
#[derive(Debug, Clone)]
pub struct FooterSegment {
    pub plain: String,
    pub colored: String,
}
#[derive(Debug, Clone)]
pub struct FooterRightLabel {
    /// Forms from richest to poorest; the final entry is the always-kept floor.
    pub forms: Vec<FooterRightForm>,
}
#[derive(Debug, Clone)]
pub struct FooterRightForm {
    pub segment: FooterSegment,
    pub fits_with: FooterRightFit,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FooterRightFit {
    Everything,
    MiddleElision,
    PwdElision,
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
        right_form: usize,
    },
    MiddleElided {
        kept_middle_count: usize,
        show_marker: bool,
        right_form: usize,
    },
    PwdElided {
        pwd_plain: String,
        kept_middle_count: usize,
        show_marker: bool,
        right_form: usize,
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
    let plan = |right: &FooterRightForm, right_form: usize| {
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
                + visible_width(&right.segment.plain)
                <= input.width
            {
                return Some(if kept_middle_count == input.middle.len() {
                    FooterLayout::Full { right_form }
                } else {
                    FooterLayout::MiddleElided {
                        kept_middle_count,
                        show_marker,
                        right_form,
                    }
                });
            }
            if right.fits_with == FooterRightFit::Everything {
                return None;
            }
            if right.fits_with == FooterRightFit::PwdElision
                && (prefer_pwd || (kept_middle_count == 0 && !show_marker))
            {
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
                        + visible_width(&right.segment.plain)
                        + visible_width(input.separator)
                        + segments_width(&rest, input.separator),
                );
                if budget >= 2 {
                    return Some(FooterLayout::PwdElided {
                        pwd_plain: elide_head(&pwd.plain, budget),
                        kept_middle_count,
                        show_marker,
                        right_form,
                    });
                }
            }
        }
        None
    };
    for (right_form, form) in input.right.forms.iter().enumerate() {
        if let Some(p) = plan(form, right_form) {
            return Ok(p);
        }
    }
    let floor = input.right.forms.last().map(|form| &form.segment).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "footer right-label ladder is empty")
    })?;
    let budget = input
        .width
        .saturating_sub(input.min_padding + visible_width(&floor.plain));
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
            right_plain: truncate_to_width(&floor.plain, input.width, "", false),
        })
    }
}
